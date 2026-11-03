use crate::models::ProbeError;

/// ICMP Type for an Echo Request (Ping)
const ICMP_TYPE_ECHO_REQUEST: u8 = 8;
/// ICMP Code for an Echo Request is always 0
const ICMP_CODE_ECHO_REQUEST: u8 = 0;

/// Represents an ICMP Echo Request Packet.
/// It strictly follows the 8-byte header structure defined in RFC 792.
#[derive(Debug)]
pub struct IcmpEchoRequest<'a> {
    pub identifier: u16,
    pub sequence_number: u16,
    pub payload: &'a [u8],
}

impl<'a> IcmpEchoRequest<'a> {
    /// Constructs a new ICMP Echo Request.
    pub fn new(identifier: u16, sequence_number: u16, payload: &'a [u8]) -> Self {
        Self {
            identifier,
            sequence_number,
            payload,
        }
    }

    /// Serializes the ICMP packet into a raw byte array (`Vec<u8>`) ready to be sent over the wire.
    /// This function handles the Big-Endian conversion and checksum calculation.
    pub fn encode(&self) -> Vec<u8> {
        let mut buffer = self.encode_without_checksum();

        // 5. Calculate the Checksum over the entire buffer
        let checksum = Self::calculate_checksum(&buffer);

        // 6. Write the calculated Checksum back into Bytes 2 and 3 (Big-Endian)
        let checksum_bytes = checksum.to_be_bytes();
        buffer[2] = checksum_bytes[0];
        buffer[3] = checksum_bytes[1];

        buffer
    }

    /// Serializes the ICMP packet into a raw byte array WITHOUT calculating the checksum.
    /// This is used for Unix DGRAM sockets where the OS handles the checksum.
    pub fn encode_without_checksum(&self) -> Vec<u8> {
        let mut buffer = vec![0u8; 8 + self.payload.len()];

        buffer[0] = ICMP_TYPE_ECHO_REQUEST;
        buffer[1] = ICMP_CODE_ECHO_REQUEST;

        // Byte 2 and 3 are for the Checksum. Leave them as 0x00.

        let id_bytes = self.identifier.to_be_bytes();
        buffer[4] = id_bytes[0];
        buffer[5] = id_bytes[1];

        let seq_bytes = self.sequence_number.to_be_bytes();
        buffer[6] = seq_bytes[0];
        buffer[7] = seq_bytes[1];

        if !self.payload.is_empty() {
            buffer[8..].copy_from_slice(self.payload);
        }

        buffer
    }

    /// Calculates the Internet Checksum as specified in RFC 1071.
    /// It uses a 32-bit accumulator to sum 16-bit words, folds the carries,
    /// and performs a bitwise NOT (1's complement) at the end.
    fn calculate_checksum(buffer: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        let mut i = 0;

        // Sum adjacent bytes as 16-bit words
        while i + 1 < buffer.len() {
            let word = (buffer[i] as u32) << 8 | (buffer[i + 1] as u32);
            sum += word;
            i += 2;
        }

        // If the payload length is odd, pad the last byte with zero
        if !buffer.len().is_multiple_of(2) {
            let word = (buffer[buffer.len() - 1] as u32) << 8;
            sum += word;
        }

        // Fold the 32-bit sum into 16 bits by adding the carry (top 16 bits) to the lower 16 bits.
        // This might produce another carry, so we do it twice to be safe.
        while (sum >> 16) > 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }

        // Apply bitwise NOT (1's complement) and cast down to 16 bits
        !sum as u16
    }
}

pub const ICMP_TYPE_ECHO_REPLY: u8 = 0;
pub const ICMP_TYPE_DEST_UNREACHABLE: u8 = 3;
pub const ICMP_TYPE_TIME_EXCEEDED: u8 = 11;

/// Represents an incoming ICMP Echo Reply Packet.
#[derive(Debug)]
pub struct IcmpEchoReply {
    pub type_: u8,
    pub code: u8,
    pub identifier: u16,
    pub sequence_number: u16,
}

impl IcmpEchoReply {
    /// Decodes a raw byte buffer into an IcmpEchoReply struct.
    /// Returns an Error if the buffer is too small or malformed.
    pub fn decode(buffer: &[u8]) -> Result<Self, ProbeError> {
        // An ICMP header must be at least 8 bytes long.
        if buffer.len() < 8 {
            tracing::error!("Buffer too short to contain a valid ICMP header");
            return Err(ProbeError::PacketError(
                "Buffer too short to contain a valid ICMP header".to_string(),
            ));
        }

        let type_ = buffer[0];
        let code = buffer[1];

        // Only parse if it's actually an Echo Reply.
        // (Note: Unprivileged datagram sockets usually strip the IP header,
        // so byte 0 is the start of the ICMP header).
        if type_ != ICMP_TYPE_ECHO_REPLY {
            tracing::error!("Not an ICMP Echo Reply. Type: {}", type_);
            return Err(ProbeError::PacketError(
                "Not an ICMP Echo Reply".to_string(),
            ));
        }

        // Reconstruct the 16-bit Identifier from Network Byte Order (Big-Endian)
        let identifier = u16::from_be_bytes([buffer[4], buffer[5]]);

        // Reconstruct the 16-bit Sequence Number from Network Byte Order (Big-Endian)
        let sequence_number = u16::from_be_bytes([buffer[6], buffer[7]]);

        Ok(Self {
            type_,
            code,
            identifier,
            sequence_number,
        })
    }
}

#[derive(Debug)]
pub enum IcmpResponse {
    EchoReply(IcmpEchoReply),
    TimeExceeded(IcmpTimeExceeded),
    DestinationUnreachable(IcmpDestUnreachable),
    Unknown { type_: u8, code: u8 },
}

#[derive(Debug)]
pub struct IcmpTimeExceeded {
    pub code: u8,
    pub original_identifier: u16,
    pub original_sequence: u16,
}

#[derive(Debug)]
pub struct IcmpDestUnreachable {
    pub code: u8,
    pub original_identifier: u16,
    pub original_sequence: u16,
}

impl IcmpResponse {
    pub fn strip_ipv4_header(buffer: &[u8]) -> &[u8] {
        if !buffer.is_empty() && (buffer[0] >> 4) == 4 {
            // IPv4 check
            let ihl = (buffer[0] & 0x0F) as usize;
            let header_len = ihl * 4;
            if buffer.len() >= header_len {
                return &buffer[header_len..];
            }
        }
        buffer
    }

    pub fn decode(buffer: &[u8]) -> Result<Self, ProbeError> {
        if buffer.len() < 8 {
            tracing::error!("Buffer too short to contain a valid ICMP response header");
            return Err(ProbeError::PacketError("Buffer too short".to_string()));
        }
        tracing::debug!("Received ICMP Type: {}, Code: {}", buffer[0], buffer[1]);
        match buffer[0] {
            ICMP_TYPE_ECHO_REPLY => Ok(IcmpResponse::EchoReply(IcmpEchoReply::decode(buffer)?)),
            ICMP_TYPE_TIME_EXCEEDED => Ok(IcmpResponse::TimeExceeded(Self::decode_time_exceeded(
                buffer,
            )?)),
            ICMP_TYPE_DEST_UNREACHABLE => Ok(IcmpResponse::DestinationUnreachable(
                Self::decode_dest_unreachable(buffer)?,
            )),
            t => Ok(IcmpResponse::Unknown {
                type_: t,
                code: buffer[1],
            }),
        }
    }

    fn decode_time_exceeded(buffer: &[u8]) -> Result<IcmpTimeExceeded, ProbeError> {
        let (identifier, sequence) = Self::decode_error_body(buffer)?;
        Ok(IcmpTimeExceeded {
            code: buffer[1],
            original_identifier: identifier,
            original_sequence: sequence,
        })
    }

    fn decode_dest_unreachable(buffer: &[u8]) -> Result<IcmpDestUnreachable, ProbeError> {
        let (identifier, sequence) = Self::decode_error_body(buffer)?;
        Ok(IcmpDestUnreachable {
            code: buffer[1],
            original_identifier: identifier,
            original_sequence: sequence,
        })
    }

    fn decode_error_body(buffer: &[u8]) -> Result<(u16, u16), ProbeError> {
        // Buffer layout (DGRAM/RAW ICMP payload):
        // [0..8] ICMP Header (Time Exceeded / Dest Unreachable)
        // [8..X] Original IP Header (usually 20 bytes)
        // [X..X+8] Original ICMP Header
        if buffer.len() < 36 {
            // 8 + 20 + 8
            tracing::error!("Buffer too short to contain original IP and ICMP headers");
            return Err(ProbeError::PacketError(
                "Buffer too short to contain original IP and ICMP headers".to_string(),
            ));
        }

        // Ensure it's an IPv4 header by checking version (first 4 bits)
        let ip_version = buffer[8] >> 4;
        if ip_version != 4 {
            tracing::error!("Original IP header is not IPv4");
            return Err(ProbeError::PacketError(
                "Original IP header is not IPv4".to_string(),
            ));
        }

        let ihl = (buffer[8] & 0x0F) as usize;
        let ip_header_len = ihl * 4;

        if buffer.len() < 8 + ip_header_len + 8 {
            tracing::error!("Buffer too short based on original IHL");
            return Err(ProbeError::PacketError(
                "Buffer too short based on original IHL".to_string(),
            ));
        }

        // Offset 9 in IP header is the Protocol field
        if buffer[8 + 9] != 1
        /* ICMP */
        {
            tracing::error!("Original protocol was not ICMP");
            return Err(ProbeError::PacketError(
                "Original protocol was not ICMP".to_string(),
            ));
        }

        let orig_icmp_offset = 8 + ip_header_len;

        // Extract identifier and sequence from original ICMP header
        // Identifier is at offset 4, Sequence is at offset 6
        let identifier =
            u16::from_be_bytes([buffer[orig_icmp_offset + 4], buffer[orig_icmp_offset + 5]]);
        let sequence =
            u16::from_be_bytes([buffer[orig_icmp_offset + 6], buffer[orig_icmp_offset + 7]]);

        tracing::debug!(
            "Parsed inner payload - Original ID: {}, Original Seq: {}",
            identifier,
            sequence
        );

        Ok((identifier, sequence))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: u16 = 0xBEEF;
    const SEQ: u16 = 42;

    /// Builds an ICMP error message (Time Exceeded / Dest Unreachable) quoting our original
    /// Echo Request: 8-byte ICMP header + IPv4 header (`ihl` words) + original 8-byte ICMP header.
    fn error_packet(icmp_type: u8, code: u8, ihl: u8, protocol: u8) -> Vec<u8> {
        let mut buf = vec![icmp_type, code, 0, 0, 0, 0, 0, 0];
        let mut ip = vec![0u8; ihl as usize * 4];
        ip[0] = 0x40 | ihl;
        ip[9] = protocol;
        buf.extend(ip);
        buf.extend(IcmpEchoRequest::new(ID, SEQ, &[]).encode());
        buf
    }

    #[test]
    fn encode_writes_header_fields_big_endian() {
        let bytes = IcmpEchoRequest::new(0x1234, 0x5678, &[0xAA]).encode();
        assert_eq!(bytes.len(), 9);
        assert_eq!(bytes[0], ICMP_TYPE_ECHO_REQUEST);
        assert_eq!(bytes[1], ICMP_CODE_ECHO_REQUEST);
        assert_eq!(&bytes[4..8], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(bytes[8], 0xAA);
    }

    #[test]
    fn encoded_packet_checksum_verifies_to_zero() {
        // RFC 1071: summing a packet including its checksum field yields 0.
        for payload in [&[][..], &[1, 2, 3][..], &[0xFF; 32][..]] {
            let bytes = IcmpEchoRequest::new(ID, SEQ, payload).encode();
            assert_eq!(
                IcmpEchoRequest::calculate_checksum(&bytes),
                0,
                "payload {payload:?}"
            );
        }
    }

    #[test]
    fn checksum_matches_rfc1071_example() {
        // RFC 1071 section 3 example data; the one's-complement sum is 0xDDF2.
        let data = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7];
        assert_eq!(IcmpEchoRequest::calculate_checksum(&data), !0xDDF2u16);
    }

    #[test]
    fn checksum_handles_odd_and_empty_buffers() {
        assert_eq!(IcmpEchoRequest::calculate_checksum(&[0x01]), !0x0100u16);
        assert_eq!(IcmpEchoRequest::calculate_checksum(&[]), 0xFFFF);
    }

    #[test]
    fn encode_without_checksum_leaves_checksum_zero() {
        let bytes = IcmpEchoRequest::new(ID, SEQ, &[]).encode_without_checksum();
        assert_eq!(&bytes[2..4], &[0, 0]);
    }

    #[test]
    fn echo_reply_decode_reads_id_and_sequence() {
        let mut bytes = IcmpEchoRequest::new(ID, SEQ, &[]).encode();
        bytes[0] = ICMP_TYPE_ECHO_REPLY;
        let reply = IcmpEchoReply::decode(&bytes).unwrap();
        assert_eq!((reply.identifier, reply.sequence_number), (ID, SEQ));
    }

    #[test]
    fn echo_reply_decode_rejects_short_and_wrong_type() {
        assert!(IcmpEchoReply::decode(&[0; 7]).is_err());
        let request = IcmpEchoRequest::new(ID, SEQ, &[]).encode();
        assert!(IcmpEchoReply::decode(&request).is_err());
    }

    #[test]
    fn strip_ipv4_header_removes_header_by_ihl() {
        let mut buf = vec![0u8; 24];
        buf[0] = 0x46; // IPv4, IHL = 6 words = 24 bytes
        buf.extend([ICMP_TYPE_ECHO_REPLY, 0]);
        assert_eq!(
            IcmpResponse::strip_ipv4_header(&buf),
            &[ICMP_TYPE_ECHO_REPLY, 0]
        );
    }

    #[test]
    fn strip_ipv4_header_keeps_bare_icmp() {
        let icmp = [ICMP_TYPE_ECHO_REPLY, 0, 0, 0];
        assert_eq!(IcmpResponse::strip_ipv4_header(&icmp), &icmp);
        assert_eq!(IcmpResponse::strip_ipv4_header(&[]), &[] as &[u8]);
    }

    #[test]
    fn decode_time_exceeded_extracts_original_id_and_sequence() {
        let packet = error_packet(ICMP_TYPE_TIME_EXCEEDED, 0, 5, 1);
        match IcmpResponse::decode(&packet).unwrap() {
            IcmpResponse::TimeExceeded(t) => {
                assert_eq!(
                    (t.code, t.original_identifier, t.original_sequence),
                    (0, ID, SEQ)
                );
            }
            other => panic!("expected TimeExceeded, got {other:?}"),
        }
    }

    #[test]
    fn decode_dest_unreachable_honors_ip_options() {
        // IHL 6 = IPv4 header with 4 bytes of options.
        let packet = error_packet(ICMP_TYPE_DEST_UNREACHABLE, 3, 6, 1);
        match IcmpResponse::decode(&packet).unwrap() {
            IcmpResponse::DestinationUnreachable(d) => {
                assert_eq!(
                    (d.code, d.original_identifier, d.original_sequence),
                    (3, ID, SEQ)
                );
            }
            other => panic!("expected DestinationUnreachable, got {other:?}"),
        }
    }

    #[test]
    fn decode_error_body_rejects_non_icmp_or_truncated() {
        let udp = error_packet(ICMP_TYPE_TIME_EXCEEDED, 0, 5, 17);
        assert!(IcmpResponse::decode(&udp).is_err());

        let full = error_packet(ICMP_TYPE_TIME_EXCEEDED, 0, 5, 1);
        assert!(IcmpResponse::decode(&full[..full.len() - 1]).is_err());
    }

    #[test]
    fn decode_unknown_type_is_not_an_error() {
        let packet = [5, 1, 0, 0, 0, 0, 0, 0]; // ICMP Redirect
        assert!(matches!(
            IcmpResponse::decode(&packet),
            Ok(IcmpResponse::Unknown { type_: 5, code: 1 })
        ));
    }
}
