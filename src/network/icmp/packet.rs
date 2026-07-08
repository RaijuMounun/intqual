/// ICMP Type for an Echo Request (Ping)
const ICMP_TYPE_ECHO_REQUEST: u8 = 8;
/// ICMP Code for an Echo Request is always 0
const ICMP_CODE_ECHO_REQUEST: u8 = 0;

/// Represents an ICMP Echo Request Packet.
/// It strictly follows the 8-byte header structure defined in RFC 792.
#[derive(Debug)]
pub struct IcmpEchoRequest {
    pub identifier: u16,
    pub sequence_number: u16,
    pub payload: Vec<u8>,
}

impl IcmpEchoRequest {
    /// Constructs a new ICMP Echo Request.
    pub fn new(identifier: u16, sequence_number: u16, payload: Vec<u8>) -> Self {
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
            buffer[8..].copy_from_slice(&self.payload);
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
        while i < buffer.len() - 1 {
            let word = (buffer[i] as u32) << 8 | (buffer[i + 1] as u32);
            sum += word;
            i += 2;
        }

        // If the payload length is odd, pad the last byte with zero
        if buffer.len() % 2 != 0 {
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
    pub fn decode(buffer: &[u8]) -> Result<Self, &'static str> {
        // An ICMP header must be at least 8 bytes long.
        if buffer.len() < 8 {
            return Err("Buffer too short to contain a valid ICMP header");
        }

        let type_ = buffer[0];
        let code = buffer[1];

        // Only parse if it's actually an Echo Reply.
        // (Note: Unprivileged datagram sockets usually strip the IP header, 
        // so byte 0 is the start of the ICMP header).
        if type_ != ICMP_TYPE_ECHO_REPLY {
            return Err("Not an ICMP Echo Reply");
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
        if !buffer.is_empty() && (buffer[0] >> 4) == 4 { // IPv4 check
            let ihl = (buffer[0] & 0x0F) as usize;
            let header_len = ihl * 4;
            if buffer.len() >= header_len {
                return &buffer[header_len..];
            }
        }
        buffer
    }

    pub fn decode(buffer: &[u8]) -> Result<Self, &'static str> {
        if buffer.len() < 8 {
            return Err("Buffer too short");
        }
        tracing::debug!("Received ICMP Type: {}, Code: {}", buffer[0], buffer[1]);
        match buffer[0] {
            ICMP_TYPE_ECHO_REPLY => Ok(IcmpResponse::EchoReply(IcmpEchoReply::decode(buffer)?)),
            ICMP_TYPE_TIME_EXCEEDED => Ok(IcmpResponse::TimeExceeded(Self::decode_time_exceeded(buffer)?)),
            ICMP_TYPE_DEST_UNREACHABLE => Ok(IcmpResponse::DestinationUnreachable(Self::decode_dest_unreachable(buffer)?)),
            t => Ok(IcmpResponse::Unknown { type_: t, code: buffer[1] }),
        }
    }

    fn decode_time_exceeded(buffer: &[u8]) -> Result<IcmpTimeExceeded, &'static str> {
        let (identifier, sequence) = Self::decode_error_body(buffer)?;
        Ok(IcmpTimeExceeded {
            code: buffer[1],
            original_identifier: identifier,
            original_sequence: sequence,
        })
    }

    fn decode_dest_unreachable(buffer: &[u8]) -> Result<IcmpDestUnreachable, &'static str> {
        let (identifier, sequence) = Self::decode_error_body(buffer)?;
        Ok(IcmpDestUnreachable {
            code: buffer[1],
            original_identifier: identifier,
            original_sequence: sequence,
        })
    }

    fn decode_error_body(buffer: &[u8]) -> Result<(u16, u16), &'static str> {
        // Buffer layout (DGRAM/RAW ICMP payload):
        // [0..8] ICMP Header (Time Exceeded / Dest Unreachable)
        // [8..X] Original IP Header (usually 20 bytes)
        // [X..X+8] Original ICMP Header
        if buffer.len() < 36 { // 8 + 20 + 8
            tracing::debug!("Buffer too short for inner payload");
            return Err("Buffer too short to contain original IP and ICMP headers");
        }
        
        // Ensure it's an IPv4 header by checking version (first 4 bits)
        let ip_version = buffer[8] >> 4;
        if ip_version != 4 {
            return Err("Original IP header is not IPv4");
        }
        
        let ihl = (buffer[8] & 0x0F) as usize;
        let ip_header_len = ihl * 4;
        
        if buffer.len() < 8 + ip_header_len + 8 {
            tracing::debug!("Buffer too short for inner payload");
            return Err("Buffer too short based on original IHL");
        }
        
        // Offset 9 in IP header is the Protocol field
        if buffer[8 + 9] != 1 /* ICMP */ {
            return Err("Original protocol was not ICMP");
        }
        
        let orig_icmp_offset = 8 + ip_header_len;
        
        // Extract identifier and sequence from original ICMP header
        // Identifier is at offset 4, Sequence is at offset 6
        let identifier = u16::from_be_bytes([buffer[orig_icmp_offset + 4], buffer[orig_icmp_offset + 5]]);
        let sequence = u16::from_be_bytes([buffer[orig_icmp_offset + 6], buffer[orig_icmp_offset + 7]]);
        
        tracing::debug!("Parsed inner payload - Original ID: {}, Original Seq: {}", identifier, sequence);

        Ok((identifier, sequence))
    }
}