use super::nvme_defs::*;
use super::nvme_driver_defs::*;
use super::nvme_queue::NvmeQueue;
use super::DeviceData;
use super::MappingData;
use super::NvmeCommand;
use super::NvmeNamespace;
use super::NvmeRequest;
use alloc::boxed::Box;
use core;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use kernel::bindings;
use kernel::blk::mq;
use kernel::device::RawDevice;
use kernel::error::code::*;
use kernel::pr_info;
use kernel::prelude::*;
use kernel::sync::Ref;
use kernel::sync::RefBorrow;
use kernel::AtomicOptionalBoxedPtr;
use nvme_prp::*;

mod nvme_prp;

pub(crate) struct AdminQueueOperations;

#[kernel::macros::vtable]
impl mq::Operations for AdminQueueOperations {
    type RequestData = NvmeRequest;
    type QueueData = Box<NvmeNamespace>;
    type HwData = Ref<NvmeQueue<Self>>;
    type TagSetData = Ref<DeviceData>;

    fn new_request_data(
        tagset_data: <Self::TagSetData as kernel::PointerWrapper>::Borrowed<'_>,
    ) -> Result<Self::RequestData> {
        Ok(NvmeRequest {
            dma_addr: AtomicU64::new(!0),
            result: AtomicU32::new(0),
            status: AtomicU16::new(0),
            direction: AtomicU32::new(bindings::dma_data_direction_DMA_FROM_DEVICE),
            len: AtomicU32::new(0),
            dev: tagset_data.dev.clone(),
            cmd: UnsafeCell::new(NvmeCommand::default()),
            sg_count: AtomicU32::new(0),
            page_count: AtomicU32::new(0),
            first_dma: AtomicU64::new(0),
            mapping_data: AtomicOptionalBoxedPtr::new(None),
            dma_pool: tagset_data.dma_pool.clone(),
        })
    }

    fn queue_rq(
        hw_data: <Self::HwData as kernel::PointerWrapper>::Borrowed<'_>,
        queue_data: <Self::QueueData as kernel::PointerWrapper>::Borrowed<'_>,
        rq: &mq::Request<Self>,
        is_last: bool,
    ) -> Result {
        queue_rq(hw_data, queue_data, rq, is_last)
    }

    fn complete(rq: &mq::Request<Self>) {
        complete(rq)
    }

    fn commit_rqs(
        queue: <Self::HwData as kernel::PointerWrapper>::Borrowed<'_>,
        _ns: <Self::QueueData as kernel::PointerWrapper>::Borrowed<'_>,
    ) {
        queue.write_sq_db(true);
    }

    fn init_hctx(
        tagset_data: <Self::TagSetData as kernel::PointerWrapper>::Borrowed<'_>,
        _hctx_idx: u32,
    ) -> Result<Self::HwData> {
        let queues = tagset_data.queues.lock();
        Ok(queues.admin.as_ref().ok_or(EINVAL)?.clone())
    }
}

pub(crate) struct IoQueueOperations;

#[kernel::macros::vtable]
impl mq::Operations for IoQueueOperations {
    type RequestData = NvmeRequest;
    type QueueData = Box<NvmeNamespace>;
    type HwData = Ref<NvmeQueue<Self>>;
    type TagSetData = Ref<DeviceData>;

    fn new_request_data(data: RefBorrow<'_, DeviceData>) -> Result<NvmeRequest> {
        Ok(NvmeRequest {
            dma_addr: AtomicU64::new(!0),
            result: AtomicU32::new(0),
            status: AtomicU16::new(0),
            direction: AtomicU32::new(bindings::dma_data_direction_DMA_FROM_DEVICE),
            len: AtomicU32::new(0),
            dev: data.dev.clone(),
            cmd: UnsafeCell::new(NvmeCommand::default()),
            sg_count: AtomicU32::new(0),
            page_count: AtomicU32::new(0),
            first_dma: AtomicU64::new(0),
            mapping_data: AtomicOptionalBoxedPtr::new(None),
            dma_pool: data.dma_pool.clone(),
        })
    }

    fn init_hctx(
        tagset_data: RefBorrow<'_, DeviceData>,
        hctx_idx: u32,
    ) -> Result<Ref<NvmeQueue<Self>>> {
        let queues = tagset_data.queues.lock();
        Ok(queues.io[hctx_idx as usize].clone())
    }

    fn queue_rq(
        io_queue: RefBorrow<'_, NvmeQueue<Self>>,
        ns: &NvmeNamespace,
        rq: &mq::Request<Self>,
        is_last: bool,
    ) -> Result {
        queue_rq(io_queue, ns, rq, is_last)
    }

    fn complete(rq: &mq::Request<Self>) {
        complete(rq)
    }

    fn commit_rqs(io_queue: RefBorrow<'_, NvmeQueue<Self>>, _ns: &NvmeNamespace) {
        io_queue.write_sq_db(true);
    }

    fn poll(queue: RefBorrow<'_, NvmeQueue<Self>>) -> i32 {
        queue.process_completions()
    }

    fn map_queues(tag_set: &mq::TagSetRef) -> Result {
        use kernel::PointerWrapper;
        // TODO: Build abstractions for these unsafe calls
        unsafe {
            let device_data: Self::TagSetData =
                Self::TagSetData::from_pointer((*tag_set.ptr()).driver_data);
            let num_maps = (*tag_set.ptr()).nr_maps;
            pr_info!("num_maps: {}\n", num_maps);
            let mut queue_offset: u32 = 0;
            let mut irq_offset: u32 = 1; //TODO: Unless we only have 1 vector
            for i in 0..num_maps {
                let queue_count = match i {
                    bindings::hctx_type_HCTX_TYPE_DEFAULT => device_data.irq_queue_count,
                    bindings::hctx_type_HCTX_TYPE_POLL => device_data.poll_queue_count,
                    _ => 0,
                };
                let mut map = &mut (&mut (*tag_set.ptr()).map)[i as usize];
                map.nr_queues = queue_count;
                if queue_count == 0 {
                    continue;
                }
                map.queue_offset = queue_offset;
                if i != bindings::hctx_type_HCTX_TYPE_POLL && irq_offset != 0 {
                    bindings::blk_mq_pci_map_queues(
                        map,
                        device_data.pci_dev.as_ptr(),
                        irq_offset as i32,
                    );
                } else {
                    bindings::blk_mq_map_queues(map);
                }
                queue_offset += queue_count;
                irq_offset += queue_count;
            }
        }
        pr_info!("Return from map queues");
        Ok(())
    }
}

fn queue_rq<T>(
    io_queue: RefBorrow<'_, NvmeQueue<T>>,
    ns: &NvmeNamespace,
    rq: &mq::Request<T>,
    is_last: bool,
) -> Result
where
    T: mq::Operations<RequestData = NvmeRequest>,
{
    match rq.command() {
        bindings::req_opf_REQ_OP_DRV_IN | bindings::req_opf_REQ_OP_DRV_OUT => {
            io_queue.submit_command(unsafe { &*rq.data().cmd.get() }, is_last);
            Ok(())
        }
        bindings::req_opf_REQ_OP_FLUSH => {
            let mut cmd = NvmeCommand::new_flush(ns.id);
            cmd.common.command_id = rq.tag() as u16;
            io_queue.submit_command(&cmd, is_last);
            Ok(())
        }
        bindings::req_opf_REQ_OP_WRITE | bindings::req_opf_REQ_OP_READ => {
            let (direction, opcode) = if rq.command() == bindings::req_opf_REQ_OP_READ {
                (
                    bindings::dma_data_direction_DMA_FROM_DEVICE,
                    NvmeOpcode::read,
                )
            } else {
                (
                    bindings::dma_data_direction_DMA_TO_DEVICE,
                    NvmeOpcode::write,
                )
            };
            let len = rq.payload_bytes();
            // TODO: Return this from the request.
            let offset = unsafe { (*rq.bio()).bi_iter.bi_sector };
            let mut cmd = NvmeCommand {
                rw: NvmeRw {
                    opcode: opcode as _,
                    command_id: rq.tag() as u16,
                    nsid: ns.id.into(),
                    slba: (offset >> (ns.lba_shift - bindings::SECTOR_SHIFT)).into(),
                    length: ((len >> ns.lba_shift) as u16 - 1).into(),
                    ..NvmeRw::default()
                },
            };

            if rq.nr_phys_segments() == 1 {
                let bv = rq.first_bvec();
                if (bv.bv_offset % (NVME_CTRL_PAGE_SIZE as u32)) + len
                    <= (NVME_CTRL_PAGE_SIZE * 2) as u32
                {
                    let dma_addr = unsafe {
                        bindings::dma_map_page_attrs(
                            io_queue.data.dev.ptr,
                            bv.bv_page,
                            bv.bv_offset as _,
                            len as _,
                            direction,
                            0,
                        )
                    };
                    if dma_addr == !0 {
                        return Err(ENOMEM);
                    }

                    rq.start();

                    cmd.rw.prp1 = dma_addr.into();
                    if len > (NVME_CTRL_PAGE_SIZE as u32) {
                        cmd.rw.prp2 = (dma_addr + (NVME_CTRL_PAGE_SIZE as u64)).into();
                    }

                    let pdu = rq.data();
                    pdu.dma_addr.store(dma_addr, Ordering::Relaxed);
                    pdu.direction.store(direction, Ordering::Relaxed);
                    pdu.len.store(len, Ordering::Relaxed);

                    io_queue.submit_command(&cmd, is_last);
                    return Ok(());
                }
            }

            use kernel::box_ext::BoxExt;
            let mut md = Box::try_new_atomic(MappingData::default())?;
            let count = rq.map_sg(&mut md.sg)?;
            let dev = &io_queue.data.dev;
            dev.dma_map_sg(&mut md.sg[..count as usize], direction)?;
            let page_count = setup_prps(&io_queue.data, &mut cmd, &mut md, len)?;

            let pdu = rq.data();
            pdu.sg_count.store(count, Ordering::Relaxed);
            pdu.page_count.store(page_count, Ordering::Relaxed);
            pdu.first_dma
                .store(unsafe { cmd.common.prp2.into() }, Ordering::Relaxed);
            pdu.mapping_data.store(Some(md), Ordering::Relaxed);

            rq.start();

            io_queue.submit_command(&cmd, is_last);
            Ok(())
        }

        _ => Err(EIO),
    }
}

fn complete<T>(rq: &mq::Request<T>)
where
    T: mq::Operations<RequestData = NvmeRequest>,
{
    match rq.command() {
        bindings::req_opf_REQ_OP_DRV_IN
        | bindings::req_opf_REQ_OP_DRV_OUT
        | bindings::req_opf_REQ_OP_FLUSH => {
            // We just complete right away if flush completes.
            rq.end_ok();
            return;
        }
        _ => {}
    }

    let pdu = rq.data();

    if let Some(mut md) = pdu.mapping_data.take(Ordering::Relaxed) {
        pdu.dev.dma_unmap_sg(
            &mut md.sg[..pdu.sg_count.load(Ordering::Relaxed) as usize],
            pdu.direction.load(Ordering::Relaxed),
        );
        free_prps(
            pdu.page_count.load(Ordering::Relaxed) as _,
            &md.pages,
            pdu.first_dma.load(Ordering::Relaxed),
            &pdu.dma_pool,
        );
    } else {
        // Unmap the page we mapped.
        unsafe {
            bindings::dma_unmap_page_attrs(
                pdu.dev.ptr,
                pdu.dma_addr.load(Ordering::Relaxed),
                pdu.len.load(Ordering::Relaxed) as _,
                pdu.direction.load(Ordering::Relaxed),
                0,
            )
        };
    }

    // On failure, complete the request immediately with an error.
    let status = pdu.status.load(Ordering::Relaxed);
    if status != 0 {
        pr_info!("Completing with error {:x}\n", status);
        rq.end_err(EIO);
        return;
    }

    rq.end_ok();
}
