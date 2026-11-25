// SPDX-License-Identifier: MIT
//! The media device's interrupt (fix/codec-irq, 2026-11-25), as the staged DroidVMVirtioMedia.c (edit 7) uses it: the
//! command queue with VIRTQ_AVAIL_F_NO_INTERRUPT raises nothing (xnuhost answers inside the notify, the guest reads the
//! used ring at once), the event queue without it raises the SPI when the device's worker hands an event back, and the
//! acknowledgement (0x64) takes the line down. With oneios-codec-irq=0 (both queues NO_INTERRUPT) the line stays down
//! and the events still come. A binary of its own: the SPI recorder counts per line in the whole process, and every
//! encctl Guest is on SPI 40.
use oneencoder::backend_stub::StubBackend;
use oneencoder::decoder::StubDecodeBackend;
use oneencoder::encctl::{querybuf, Guest};
use oneencoder::pod::Pod;
use oneencoder::proto::*;
use std::sync::Arc;
use std::time::Duration;
use xh_core::bytes::le;
use xh_core::virtio::testing;

const W: u32 = 640;
const H: u32 = 360;

fn ack(g: &Guest) {
    let isr = g.dev.mmio_read(0x60, 4);
    g.dev.mmio_write(0x64, 4, isr);
}

/// OPEN, S_FMT and REQBUFS of OUTPUT in H.264, each buffer's MMAP, STREAMON: the device's decoder opens and its worker
/// hands back a SOURCE_CHANGE on the event queue. The session.
fn start(g: &mut Guest) -> u32 {
    let session = g.open().expect("OPEN");
    let mut subscription = [MEDIA_EVENT_SOURCE_CHANGE, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_SUBSCRIBE_EVENT, subscription.as_bytes_mut()), 0);
    let mut out = MediaV4l2Format {
        type_: MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
        width: W,
        height: H,
        pixelformat: MEDIA_PIXFMT_H264,
        num_planes: 1,
        ..Default::default()
    };
    out.planes[0].sizeimage = 2 << 20;
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_S_FMT, out.as_bytes_mut()), 0);
    let mut req = [4, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, MEDIA_MEMORY_MMAP, 0, 0];
    assert!(g.ioctl(session, MEDIA_VIDIOC_REQBUFS, req.as_bytes_mut()) == 0 && req[0] == 4);
    for i in 0..4 {
        let b = querybuf(g, session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, i);
        g.mmap(session, b.plane.m).expect("MMAP of an OUTPUT buffer");
    }
    assert_eq!(g.stream_on(session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE), 0);
    session
}

fn source_change(g: &mut Guest) {
    let (kind, _, ev) = g.wait_event(Duration::from_secs(3)).expect("the SOURCE_CHANGE");
    assert_eq!((kind, le::u32(&ev, 8)), (MEDIA_EVT_EVENT, MEDIA_EVENT_SOURCE_CHANGE));
}

#[test]
fn the_event_queue_raises_the_spi_and_the_command_queue_does_not() {
    let mut g =
        Guest::up_decoding(Arc::new(StubBackend), |pool, _| Arc::new(StubDecodeBackend::new(pool)), 0x0b10_0000)
            .expect("the device and its guest");
    let spi = g.dev.spi();
    assert_eq!(spi, 40);

    // oneios-codec-irq=1: the command queue NO_INTERRUPT, the event queue on the interrupt
    g.set_no_interrupt(MEDIA_QUEUE_COMMAND, true);
    g.set_no_interrupt(MEDIA_QUEUE_EVENT, false);
    ack(&g);
    assert!(!testing::spi_level(spi));
    let raised = testing::spi_raised(spi);
    let session = g.open().expect("OPEN");
    assert_eq!(
        (testing::spi_raised(spi), testing::spi_level(spi), g.dev.mmio_read(0x60, 4)),
        (raised, false, 0),
        "a command answered inside the notify raises nothing"
    );
    assert_eq!(g.close(session), Some(0));
    let session = start(&mut g);
    source_change(&mut g);
    // the commands raised nothing; the event did, and its status says a used buffer
    assert_eq!(testing::spi_raised(spi), raised + 1, "one interrupt: the event's");
    assert!(testing::spi_level(spi));
    assert_eq!(g.dev.mmio_read(0x60, 4) & 1, 1);
    ack(&g);
    assert!(!testing::spi_level(spi), "the acknowledgement takes the level down");
    assert_eq!(g.close(session), Some(0));

    // oneios-codec-irq=0: both queues NO_INTERRUPT; the line stays down and the event still comes (the guest polls)
    g.set_no_interrupt(MEDIA_QUEUE_EVENT, true);
    let raised = testing::spi_raised(spi);
    let session = start(&mut g);
    source_change(&mut g);
    assert_eq!((testing::spi_raised(spi), testing::spi_level(spi), g.dev.mmio_read(0x60, 4)), (raised, false, 0));
    assert_eq!(g.close(session), Some(0));
}
