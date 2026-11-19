//! One redirected device: serves `vhci_hcd`'s URBs from the RDP client.
//!
//! Linux enumerates the device itself through `vhci_hcd`, so most requests pass
//! straight through. The exceptions are requests that change device state, which
//! RDPEUSB requires to go through its own messages so the client's host stack
//! knows about them and can hand back pipe handles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use ironrdp_rdpeusb::io::DeviceText;
use ironrdp_usb::control::{GetDescriptorRequest, Recipient, SetupPacket};
use ironrdp_usb::descriptor::ConfigurationDescriptorSet;
use ironrdp_usb::endpoint::EndpointAddress;
use ironrdp_usb::transfer::{ControlTransferRequest, DataTransferRequest, UsbError, UsbResult};
use ironrdp_usb::InterfaceSelection;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::urbdrc::{PendingHandle, PendingRequest, RdpUsbDeviceAnnounceInfo, UsbDeviceHandle, UsbRedirDevice, UsbRequestCompletion};
use crate::usbip::{self, Command, DIR_IN, HEADER_SIZE, Submit};
use crate::vhci;

type Result<T> = core::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const EPIPE: i32 = -32;
const ENODEV: i32 = -19;
const EOVERFLOW: i32 = -75;
const EOPNOTSUPP: i32 = -95;
const ECONNRESET: i32 = -104;
const ETIMEDOUT: i32 = -110;
const EPROTO: i32 = -71;

const DESCRIPTOR_DEVICE: u8 = 1;
const DESCRIPTOR_CONFIGURATION: u8 = 2;
const DESCRIPTOR_ENDPOINT: u8 = 5;

const REQUEST_CLEAR_FEATURE: u8 = 1;
const REQUEST_GET_DESCRIPTOR: u8 = 6;
const REQUEST_SET_CONFIGURATION: u8 = 9;
const REQUEST_SET_INTERFACE: u8 = 11;

const TRANSFER_TYPE_MASK: u8 = 3;
const TRANSFER_ISOCHRONOUS: u8 = 1;
const TRANSFER_INTERRUPT: u8 = 3;

fn errno(error: UsbError) -> i32 {
    match error {
        UsbError::Cancelled => ECONNRESET,
        UsbError::Stall => EPIPE,
        UsbError::Timeout => ETIMEDOUT,
        UsbError::Overflow => EOVERFLOW,
        UsbError::NoDevice => ENODEV,
        _ => EPROTO,
    }
}

/// The USB/IP reply for a completed request.
fn reply_for(seqnum: u32, is_in: bool, length: u32, completion: UsbRequestCompletion) -> Vec<u8> {
    let status_of = |result: UsbResult<()>| result.err().map_or(0, errno);
    match completion {
        UsbRequestCompletion::Transfer(transfer) => {
            let data: &[u8] = if is_in { &transfer.data } else { &[] };
            usbip::ret_submit(seqnum, status_of(transfer.status), transfer.actual_length, data)
        }
        UsbRequestCompletion::Descriptor(Ok(data)) => {
            let actual = u32::try_from(data.len()).expect("descriptors fit in wLength");
            usbip::ret_submit(seqnum, 0, actual, &data)
        }
        UsbRequestCompletion::Descriptor(Err(error)) => usbip::ret_submit(seqnum, errno(error), 0, &[]),
        UsbRequestCompletion::Unit(result) => {
            let status = status_of(result);
            usbip::ret_submit(seqnum, status, if status == 0 && !is_in { length } else { 0 }, &[])
        }
        other => {
            tracing::warn!(?other, "Unexpected completion for a USB/IP request");
            usbip::ret_submit(seqnum, EPROTO, 0, &[])
        }
    }
}

/// Descriptors read before attaching, needed to answer state-changing requests.
#[derive(Default)]
struct Descriptors {
    /// Complete configuration descriptor sets, by `bConfigurationValue`.
    configurations: HashMap<u8, Vec<u8>>,
    /// `bmAttributes` by endpoint address, across all configurations.
    endpoints: HashMap<u8, u8>,
}

impl Descriptors {
    fn add_configuration(&mut self, value: u8, set: Vec<u8>) {
        let mut rest = set.as_slice();
        while let [length, kind, ..] = *rest {
            let length = usize::from(length);
            if length < 2 || length > rest.len() {
                break;
            }
            if kind == DESCRIPTOR_ENDPOINT && length >= 7 {
                self.endpoints.insert(rest[2], rest[3]);
            }
            rest = &rest[length..];
        }
        self.configurations.insert(value, set);
    }
}

async fn fetch_descriptor(usb: &UsbDeviceHandle, descriptor_type: u8, index: u8, length: u16) -> Result<Vec<u8>> {
    let request = GetDescriptorRequest {
        recipient: Recipient::DEVICE,
        descriptor_type,
        descriptor_index: index,
        index: 0,
        requested_length: length,
    };
    match usb.get_descriptor(request).await?.wait().await? {
        UsbRequestCompletion::Descriptor(result) => Ok(result?),
        other => Err(format!("unexpected descriptor completion {other:?}").into()),
    }
}

async fn read_descriptors(usb: &UsbDeviceHandle) -> Result<Descriptors> {
    let device = fetch_descriptor(usb, DESCRIPTOR_DEVICE, 0, 18).await?;
    let count = *device.get(17).ok_or("short device descriptor")?;

    let mut descriptors = Descriptors::default();
    for index in 0..count {
        let header = fetch_descriptor(usb, DESCRIPTOR_CONFIGURATION, index, 9).await?;
        if header.len() < 9 {
            return Err("short configuration descriptor".into());
        }
        let total = u16::from_le_bytes([header[2], header[3]]);
        let set = fetch_descriptor(usb, DESCRIPTOR_CONFIGURATION, index, total).await?;
        descriptors.add_configuration(header[5], set);
    }
    Ok(descriptors)
}

/// In-flight URBs, so that an unlink can cancel them. An entry's removal also
/// decides who replies: an URB that has been unlinked must not complete as well.
type Inflight = Arc<Mutex<HashMap<u32, PendingHandle>>>;

struct Connection {
    usb: UsbDeviceHandle,
    descriptors: Descriptors,
    active_configuration: Option<u8>,
    replies: UnboundedSender<Vec<u8>>,
    inflight: Inflight,
}

impl Connection {
    fn reply(&self, message: Vec<u8>) {
        let _ = self.replies.send(message);
    }

    async fn submit(&mut self, submit: &Submit, data: Vec<u8>) -> Result<Option<PendingRequest>> {
        if submit.endpoint == 0 {
            return self.submit_control(submit, data).await;
        }

        let raw = submit.endpoint | if submit.direction == DIR_IN { 0x80 } else { 0 };
        let endpoint = EndpointAddress::from_raw(raw)?;
        let attributes = *self.descriptors.endpoints.get(&raw).ok_or("unknown endpoint")?;
        let request = DataTransferRequest {
            endpoint,
            length: submit.length,
            data,
        };
        match attributes & TRANSFER_TYPE_MASK {
            TRANSFER_ISOCHRONOUS => Ok(None),
            TRANSFER_INTERRUPT => Ok(Some(self.usb.interrupt_transfer(request).await?)),
            _ => Ok(Some(self.usb.bulk_transfer(request).await?)),
        }
    }

    async fn submit_control(&mut self, submit: &Submit, data: Vec<u8>) -> Result<Option<PendingRequest>> {
        let setup = SetupPacket::from_bytes(submit.setup);
        let recipient = setup.request_type.recipient();

        match (setup.standard_request(), recipient) {
            (Some(REQUEST_GET_DESCRIPTOR), _) => {
                let [index, descriptor_type] = setup.value.to_le_bytes();
                let request = GetDescriptorRequest {
                    recipient,
                    descriptor_type,
                    descriptor_index: index,
                    index: setup.index,
                    requested_length: setup.length,
                };
                Ok(Some(self.usb.get_descriptor(request).await?))
            }
            (Some(REQUEST_SET_CONFIGURATION), Recipient::DEVICE) => {
                let value = setup.value as u8;
                let set = (value != 0)
                    .then(|| self.descriptors.configurations.get(&value).ok_or("unknown configuration"))
                    .transpose()?;
                let parsed = set.map(|bytes| ConfigurationDescriptorSet::parse(bytes)).transpose()?;
                let pending = self.usb.select_configuration(parsed).await?;
                self.active_configuration = (value != 0).then_some(value);
                Ok(Some(pending))
            }
            (Some(REQUEST_SET_INTERFACE), Recipient::INTERFACE) => {
                let value = self.active_configuration.ok_or("SET_INTERFACE with no configuration")?;
                let bytes = self.descriptors.configurations.get(&value).ok_or("unknown configuration")?;
                let selection = InterfaceSelection {
                    interface: setup.index as u8,
                    alternate_setting: setup.value as u8,
                };
                let parsed = ConfigurationDescriptorSet::parse(bytes)?;
                Ok(Some(self.usb.select_interface(parsed, selection).await?))
            }
            (Some(REQUEST_CLEAR_FEATURE), Recipient::ENDPOINT) if setup.value == 0 => {
                let endpoint = EndpointAddress::from_raw(setup.index as u8)?;
                Ok(Some(self.usb.clear_halt(endpoint).await?))
            }
            _ => Ok(Some(self.usb.control_transfer(ControlTransferRequest { setup, data }).await?)),
        }
    }

    async fn handle_submit(&mut self, submit: Submit, data: Vec<u8>) {
        let seqnum = submit.seqnum;
        let is_in = submit.direction == DIR_IN;

        // Isochronous transfers carry packet tables this bridge does not parse yet.
        if submit.number_of_packets != u32::MAX && submit.number_of_packets != 0 {
            self.reply(usbip::ret_submit(seqnum, EOPNOTSUPP, 0, &[]));
            return;
        }

        let pending = match self.submit(&submit, data).await {
            Ok(Some(pending)) => pending,
            Ok(None) => {
                self.reply(usbip::ret_submit(seqnum, EOPNOTSUPP, 0, &[]));
                return;
            }
            Err(error) => {
                tracing::warn!(%error, seqnum, endpoint = submit.endpoint, "Failed to submit URB");
                self.reply(usbip::ret_submit(seqnum, EPROTO, 0, &[]));
                return;
            }
        };

        // Submission stays on the read loop to keep each endpoint in order. Only
        // the wait moves to a task, since an interrupt IN can pend for minutes.
        lock(&self.inflight).insert(seqnum, pending.pending_handle());
        let inflight = Arc::clone(&self.inflight);
        let replies = self.replies.clone();
        let length = submit.length;
        tokio::spawn(async move {
            let result = pending.wait().await;
            if lock(&inflight).remove(&seqnum).is_none() {
                return; // unlinked, and the unlink has already been answered
            }
            let message = match result {
                Ok(completion) => reply_for(seqnum, is_in, length, completion),
                Err(error) => {
                    tracing::debug!(%error, seqnum, "URB failed");
                    usbip::ret_submit(seqnum, ENODEV, 0, &[])
                }
            };
            let _ = replies.send(message);
        });
    }

    fn handle_unlink(&mut self, seqnum: u32, target: u32) {
        let handle = lock(&self.inflight).remove(&target);
        let status = match handle {
            Some(handle) => {
                handle.cancel();
                ECONNRESET
            }
            None => 0, // already completed
        };
        self.reply(usbip::ret_unlink(seqnum, status));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn serve(info: RdpUsbDeviceAnnounceInfo) -> Result<()> {
    let RdpUsbDeviceAnnounceInfo { announce, usb_handle } = info;
    tracing::info!(device = %announce.device_instance_id, speed = ?announce.speed, "Device announced");

    let descriptors = read_descriptors(&usb_handle).await?;

    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    let port = vhci::attach(&theirs, announce.speed)?;
    drop(theirs); // vhci_hcd holds its own reference
    tracing::info!(port, "Device attached");

    ours.set_nonblocking(true)?;
    let (mut reader, mut writer) = UnixStream::from_std(ours)?.into_split();

    let (replies, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if writer.write_all(&message).await.is_err() {
                break;
            }
        }
    });

    let mut connection = Connection {
        usb: usb_handle,
        descriptors,
        active_configuration: None,
        replies,
        inflight: Inflight::default(),
    };

    let mut header = [0u8; HEADER_SIZE];
    loop {
        reader.read_exact(&mut header).await?;
        match usbip::parse(&header)? {
            Command::Submit(submit) => {
                let mut data = Vec::new();
                if submit.direction != DIR_IN {
                    data.resize(submit.length as usize, 0);
                    reader.read_exact(&mut data).await?;
                }
                connection.handle_submit(submit, data).await;
            }
            Command::Unlink { seqnum, target } => connection.handle_unlink(seqnum, target),
        }
    }
}

/// Dropping this detaches the device, since `vhci_hcd` sees its socket close.
#[derive(Default)]
pub struct BridgeDevice {
    task: Option<JoinHandle<()>>,
}

impl UsbRedirDevice for BridgeDevice {
    fn device_added(&mut self, info: RdpUsbDeviceAnnounceInfo) {
        self.task = Some(tokio::spawn(async move {
            if let Err(error) = serve(info).await {
                tracing::info!(%error, "Device connection ended");
            }
        }));
    }

    fn device_text(&mut self, device_text: DeviceText) {
        tracing::debug!(?device_text, "Device text");
    }

    fn close(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_endpoints_in_a_configuration() {
        let set = vec![
            9, 2, 41, 0, 1, 1, 0, 0x80, 50, // configuration
            9, 4, 0, 0, 2, 3, 0, 0, 0, // interface
            9, 0x21, 0x11, 1, 0, 1, 0x22, 34, 0, // HID
            7, 5, 0x81, 3, 64, 0, 5, // interrupt IN
            7, 5, 0x01, 3, 64, 0, 5, // interrupt OUT
        ];
        let mut descriptors = Descriptors::default();
        descriptors.add_configuration(1, set);

        assert_eq!(descriptors.endpoints.get(&0x81), Some(&TRANSFER_INTERRUPT));
        assert_eq!(descriptors.endpoints.get(&0x01), Some(&TRANSFER_INTERRUPT));
        assert!(descriptors.configurations.contains_key(&1));
    }

    #[test]
    fn a_truncated_descriptor_ends_the_walk() {
        let mut descriptors = Descriptors::default();
        descriptors.add_configuration(1, vec![9, 2, 0, 0, 0, 0, 0, 0, 0, 7, 5, 0x81]);
        assert!(descriptors.endpoints.is_empty());
    }

    #[test]
    fn stalls_map_to_epipe() {
        let completion = UsbRequestCompletion::Unit(Err(UsbError::Stall));
        let message = reply_for(1, false, 0, completion);
        assert_eq!(message[20..24], EPIPE.to_be_bytes());
    }

    #[test]
    fn successful_out_control_reports_its_length() {
        let message = reply_for(1, false, 8, UsbRequestCompletion::Unit(Ok(())));
        assert_eq!(message[24..28], 8u32.to_be_bytes());
    }
}
