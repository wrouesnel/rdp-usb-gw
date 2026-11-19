//! One RDP session: the control channel plus a channel per redirected device.
//!
//! This is the role `ironrdp-server`'s event loop plays for its own connections,
//! with the proxy socket standing in for DRDYNVC.

use std::collections::HashMap;
use std::sync::Arc;

use ironrdp_dvc::{DvcMessage, DvcProcessor as _};
use ironrdp_pdu::PduResult;
use ironrdp_rdpeusb::InterfaceAlloc;
use ironrdp_rdpeusb::server::{UrbdrcControlServer, UrbdrcDeviceServer};
use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::ServerEvent;
use crate::device::BridgeDevice;
use crate::proxy::{self, FRAME_CLOSE, FRAME_CLOSED, FRAME_DATA, FRAME_OPEN, FRAME_OPENED};
use crate::urbdrc::{
    ServerDeviceIoReq, ServerUsbDevice, UrbdrcDeviceServerMessage, UrbdrcServerMessage, UsbControlHandle,
    UsbDeviceHandle, UsbRedirServer,
};

type Result<T> = core::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const CONTROL_CHANNEL: u32 = 1;

struct Device {
    server: UrbdrcDeviceServer,
    state: Arc<ServerUsbDevice>,
}

struct Session {
    to_proxy: UnboundedSender<Vec<u8>>,
    events: UnboundedSender<ServerEvent>,
    control: UrbdrcControlServer,
    devices: HashMap<u32, Device>,
    next_channel: u32,
    iface_alloc: InterfaceAlloc,
}

pub async fn run(stream: UnixStream) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    let (to_proxy, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(frame) = outgoing.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    // Reading a frame is not cancel safe, so it cannot sit in the select below.
    let (frames_tx, mut frames) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let frame = proxy::read(&mut reader).await;
            let finished = !matches!(frame, Ok(Some(_)));
            if frames_tx.send(frame).is_err() || finished {
                break;
            }
        }
    });

    let (events, mut event_rx) = mpsc::unbounded_channel();
    let mut session = Session {
        control: UrbdrcControlServer::new(Box::new(UsbControlHandle::new(events.clone()))),
        to_proxy,
        events,
        devices: HashMap::new(),
        next_channel: CONTROL_CHANNEL + 1,
        iface_alloc: InterfaceAlloc::default(),
    };
    session.send(FRAME_OPEN, CONTROL_CHANNEL, &[]);

    let result = loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                Some(Ok(Some(frame))) => match session.handle_frame(frame) {
                    Ok(true) => {}
                    Ok(false) => break Ok(()),
                    Err(error) => break Err(error),
                },
                Some(Err(error)) => break Err(error.into()),
                Some(Ok(None)) | None => break Ok(()),
            },
            Some(ServerEvent::Usb(message)) = event_rx.recv() => {
                if let Err(error) = session.handle_usb_message(message) {
                    break Err(error);
                }
            }
        }
    };

    session.close_all_devices();
    result
}

impl Session {
    fn send(&self, kind: u32, channel: u32, payload: &[u8]) {
        // A failed send means the proxy has gone, which the reader also reports.
        let _ = self.to_proxy.send(proxy::encode(kind, channel, payload));
    }

    fn send_messages(&self, channel: u32, messages: Vec<DvcMessage>) -> Result<()> {
        for message in messages {
            let bytes = ironrdp_core::encode_vec(message.as_ref())?;
            self.send(FRAME_DATA, channel, &bytes);
        }
        Ok(())
    }

    /// Returns `false` once the session is over.
    fn handle_frame(&mut self, frame: proxy::Frame) -> Result<bool> {
        let channel = frame.channel;
        match frame.kind {
            FRAME_OPENED if channel == CONTROL_CHANNEL => {
                let messages = self.control.start(channel)?;
                self.send_messages(channel, messages)?;
            }
            FRAME_OPENED => {
                let result = self.devices.get_mut(&channel).map(|device| device.server.start(channel));
                self.finish_device_step(channel, result);
            }
            FRAME_DATA if channel == CONTROL_CHANNEL => {
                let messages = self.control.process(channel, &frame.payload)?;
                self.send_messages(channel, messages)?;
            }
            FRAME_DATA => {
                let result = self
                    .devices
                    .get_mut(&channel)
                    .map(|device| device.server.process(channel, &frame.payload));
                self.finish_device_step(channel, result);
            }
            FRAME_CLOSED if channel == CONTROL_CHANNEL => {
                tracing::info!("Client has no URBDRC channel, or closed it");
                return Ok(false);
            }
            FRAME_CLOSED => self.close_device(channel),
            kind => return Err(format!("unexpected proxy frame type {kind}").into()),
        }
        Ok(true)
    }

    /// A protocol error from one device costs that device, not the whole session.
    fn finish_device_step(&mut self, channel: u32, result: Option<PduResult<Vec<DvcMessage>>>) {
        let sent = match result {
            None => return,
            Some(result) => result.map_err(Into::into).and_then(|messages| self.send_messages(channel, messages)),
        };
        if let Err(error) = sent {
            tracing::warn!(%error, channel, "Dropping device after a protocol error");
            self.close_device(channel);
            self.send(FRAME_CLOSE, channel, &[]);
        }
    }

    fn handle_usb_message(&mut self, message: UrbdrcServerMessage) -> Result<()> {
        match message {
            UrbdrcServerMessage::AddChan => self.add_device_channel(),
            UrbdrcServerMessage::Device { dvc_id, dev_msg } => self.handle_device_message(dvc_id, dev_msg),
            UrbdrcServerMessage::DeviceClosed { dvc_id } => {
                self.devices.remove(&dvc_id);
                Ok(())
            }
        }
    }

    fn add_device_channel(&mut self) -> Result<()> {
        let channel = self.next_channel;
        self.next_channel += 1;
        let comp_iface = self.iface_alloc.alloc().ok_or("out of URBDRC interface ids")?;

        let handle = UsbDeviceHandle::new(self.events.clone(), channel);
        let state = handle.device();
        let backend = UsbRedirServer::new(Box::new(BridgeDevice::default()), handle);
        let server = UrbdrcDeviceServer::new(Box::new(backend), comp_iface)
            .map_err(|_| "allocated URBDRC interface id was invalid")?;

        self.devices.insert(channel, Device { server, state });
        self.send(FRAME_OPEN, channel, &[]);
        Ok(())
    }

    fn handle_device_message(&mut self, channel: u32, message: UrbdrcDeviceServerMessage) -> Result<()> {
        let Some(device) = self.devices.get_mut(&channel) else {
            tracing::debug!(channel, "Message for a device that has gone");
            return Ok(());
        };
        // Requests can already be queued when a retract or close changes the state.
        if !device.state.is_open() {
            return Ok(());
        }

        let mut close = false;
        let messages = match message {
            UrbdrcDeviceServerMessage::QueryDeviceText { text_type, locale_id } => {
                vec![device.server.query_device_text(text_type, locale_id)?]
            }
            UrbdrcDeviceServerMessage::IoReq { data, tx } => {
                if tx.is_closed() {
                    return Ok(());
                }
                let server = &mut device.server;
                let request = match data {
                    ServerDeviceIoReq::IoControl(packet) => server.io_control(packet),
                    ServerDeviceIoReq::InternalIoControl(packet) => server.internal_io_control(packet),
                    ServerDeviceIoReq::TransferOut(packet) => server.transfer_out(packet),
                    ServerDeviceIoReq::TransferIn(packet) => server.transfer_in(packet),
                }?;
                let pending = request
                    .expects_completion
                    .then(|| device.state.register_pending(request.request_id));
                // Answered before the write, so that a cancel cannot overtake it.
                if tx.send(pending).is_err() && request.expects_completion {
                    device.state.forget_pending(request.request_id);
                    server.abandon_unsent(request);
                    Vec::new()
                } else {
                    vec![request.message]
                }
            }
            UrbdrcDeviceServerMessage::Retract(reason) => {
                let request = device.server.retract_device(reason)?;
                device.state.mark_retracting();
                close = true;
                vec![request]
            }
            UrbdrcDeviceServerMessage::CancelRequest(request_id) => {
                if !device.state.is_pending(request_id) {
                    return Ok(());
                }
                vec![device.server.cancel_request(request_id)?]
            }
        };

        self.send_messages(channel, messages)?;
        if close {
            self.send(FRAME_CLOSE, channel, &[]);
        }
        Ok(())
    }

    fn close_device(&mut self, channel: u32) {
        if let Some(mut device) = self.devices.remove(&channel) {
            device.server.close(channel);
        }
    }

    fn close_all_devices(&mut self) {
        let channels: Vec<u32> = self.devices.keys().copied().collect();
        for channel in channels {
            self.close_device(channel);
        }
    }
}

