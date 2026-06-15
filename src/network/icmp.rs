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
        // The ICMP header is exactly 8 bytes
        let mut buffer = vec![0u8; 8 + self.payload.len()];

        // 1. Write Type and Code (Bytes 0 and 1)
        buffer[0] = ICMP_TYPE_ECHO_REQUEST;
        buffer[1] = ICMP_CODE_ECHO_REQUEST;
        
        // Byte 2 and 3 are for the Checksum. We leave them as 0x00 for now, 
        // because the checksum must be calculated over the ENTIRE packet (header + payload) 
        // while the checksum field itself is zero.

        // 2. Write Identifier (Bytes 4 and 5)
        // NETWORK BYTE ORDER RULE: We must convert from Host (Little-Endian) to Network (Big-Endian).
        let id_bytes = self.identifier.to_be_bytes();
        buffer[4] = id_bytes[0];
        buffer[5] = id_bytes[1];

        // 3. Write Sequence Number (Bytes 6 and 7)
        let seq_bytes = self.sequence_number.to_be_bytes();
        buffer[6] = seq_bytes[0];
        buffer[7] = seq_bytes[1];

        // 4. Copy the payload (if any) starting from Byte 8
        if !self.payload.is_empty() {
            buffer[8..].copy_from_slice(&self.payload);
        }

        // 5. Calculate the Checksum over the entire buffer
        let checksum = Self::calculate_checksum(&buffer);
        
        // 6. Write the calculated Checksum back into Bytes 2 and 3 (Big-Endian)
        let checksum_bytes = checksum.to_be_bytes();
        buffer[2] = checksum_bytes[0];
        buffer[3] = checksum_bytes[1];

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