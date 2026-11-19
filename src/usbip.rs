//! The USB/IP messages `vhci_hcd` exchanges with whatever owns the device. All
//! fields are big endian, and every header occupies 48 bytes.

pub const HEADER_SIZE: usize = 48;

const CMD_SUBMIT: u32 = 1;
const CMD_UNLINK: u32 = 2;
const RET_SUBMIT: u32 = 3;
const RET_UNLINK: u32 = 4;

pub const DIR_IN: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Submit(Submit),
    Unlink { seqnum: u32, target: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    pub seqnum: u32,
    pub direction: u32,
    pub endpoint: u8,
    pub length: u32,
    pub number_of_packets: u32,
    pub setup: [u8; 8],
}

fn be32(bytes: &[u8; HEADER_SIZE], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

pub fn parse(bytes: &[u8; HEADER_SIZE]) -> Result<Command, String> {
    let seqnum = be32(bytes, 4);
    match be32(bytes, 0) {
        CMD_SUBMIT => {
            let endpoint = be32(bytes, 16);
            Ok(Command::Submit(Submit {
                seqnum,
                direction: be32(bytes, 12),
                endpoint: u8::try_from(endpoint).map_err(|_| format!("bad endpoint {endpoint}"))?,
                length: be32(bytes, 24),
                // 0xffffffff marks a transfer that is not isochronous.
                number_of_packets: be32(bytes, 32),
                setup: bytes[40..48].try_into().expect("eight bytes"),
            }))
        }
        CMD_UNLINK => Ok(Command::Unlink {
            seqnum,
            target: be32(bytes, 20),
        }),
        other => Err(format!("unknown USB/IP command {other:#x}")),
    }
}

fn reply(command: u32, seqnum: u32, first: i32) -> Vec<u8> {
    let mut header = vec![0; HEADER_SIZE];
    header[0..4].copy_from_slice(&command.to_be_bytes());
    header[4..8].copy_from_slice(&seqnum.to_be_bytes());
    header[20..24].copy_from_slice(&first.to_be_bytes());
    header
}

/// `data` follows the header, and is empty for anything but a successful read.
pub fn ret_submit(seqnum: u32, status: i32, actual_length: u32, data: &[u8]) -> Vec<u8> {
    let mut message = reply(RET_SUBMIT, seqnum, status);
    message[24..28].copy_from_slice(&actual_length.to_be_bytes());
    // number_of_packets must not look like an isochronous completion.
    message[32..36].copy_from_slice(&u32::MAX.to_be_bytes());
    message.extend_from_slice(data);
    message
}

pub fn ret_unlink(seqnum: u32, status: i32) -> Vec<u8> {
    reply(RET_UNLINK, seqnum, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(fields: &[u32]) -> [u8; HEADER_SIZE] {
        let mut bytes = [0; HEADER_SIZE];
        for (i, field) in fields.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&field.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn parses_submit() {
        // command, seqnum, devid, direction, ep, flags, length, start, packets
        let mut bytes = header(&[1, 9, 0x10002, 1, 2, 0, 64, 0, u32::MAX]);
        bytes[40..48].copy_from_slice(&[0x80, 6, 0, 1, 0, 0, 18, 0]);
        assert_eq!(
            parse(&bytes),
            Ok(Command::Submit(Submit {
                seqnum: 9,
                direction: DIR_IN,
                endpoint: 2,
                length: 64,
                number_of_packets: u32::MAX,
                setup: [0x80, 6, 0, 1, 0, 0, 18, 0],
            }))
        );
    }

    #[test]
    fn parses_unlink() {
        let bytes = header(&[2, 10, 0, 0, 0, 7]);
        assert_eq!(parse(&bytes), Ok(Command::Unlink { seqnum: 10, target: 7 }));
    }

    #[test]
    fn rejects_unknown_commands() {
        assert!(parse(&header(&[99])).is_err());
    }

    #[test]
    fn encodes_ret_submit() {
        let message = ret_submit(9, -32, 2, b"ok");
        assert_eq!(message.len(), HEADER_SIZE + 2);
        assert_eq!(message[0..4], 3u32.to_be_bytes());
        assert_eq!(message[4..8], 9u32.to_be_bytes());
        assert_eq!(message[20..24], (-32i32).to_be_bytes());
        assert_eq!(message[24..28], 2u32.to_be_bytes());
        assert_eq!(&message[HEADER_SIZE..], b"ok");
    }

    #[test]
    fn encodes_ret_unlink() {
        let message = ret_unlink(10, -104);
        assert_eq!(message.len(), HEADER_SIZE);
        assert_eq!(message[0..4], 4u32.to_be_bytes());
        assert_eq!(message[20..24], (-104i32).to_be_bytes());
    }
}
