use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

use super::model::DkimTxtResolver;

#[cfg(feature = "test-support")]
use super::test_support;

/// The production resolver: a minimal DNS TXT query over UDP to the
/// nameservers in `/etc/resolv.conf` (falling back to 1.1.1.1 when none is
/// configured), with a bounded per-server timeout. It parses only what a
/// DKIM selector lookup needs — one question, TXT answers, name
/// compression — so no DNS crate enters the supply chain for this.
pub struct SystemDkimTxtResolver {
    timeout: Duration,
}

impl Default for SystemDkimTxtResolver {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
        }
    }
}

impl DkimTxtResolver for SystemDkimTxtResolver {
    fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String> {
        #[cfg(feature = "test-support")]
        if let Some(result) = test_support::lookup_txt(name) {
            return result;
        }
        let servers = system_nameservers();
        let mut last_error = String::from("no nameserver is configured");
        for server in servers {
            match query_txt(server, name, self.timeout) {
                Ok(records) => return Ok(records),
                Err(error) => last_error = error,
            }
        }
        Err(format!("DNS TXT lookup for `{name}` failed: {last_error}"))
    }
}

fn system_nameservers() -> Vec<SocketAddr> {
    let mut servers = Vec::new();
    if let Ok(body) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in body.lines() {
            let mut fields = line.split_whitespace();
            if fields.next() == Some("nameserver")
                && let Some(address) = fields.next().and_then(|field| field.parse::<IpAddr>().ok())
            {
                servers.push(SocketAddr::new(address, 53));
            }
        }
    }
    if servers.is_empty() {
        servers.push(SocketAddr::new(IpAddr::from([1, 1, 1, 1]), 53));
    }
    servers.truncate(3);
    servers
}

fn query_txt(server: SocketAddr, name: &str, timeout: Duration) -> Result<Vec<String>, String> {
    let bind: SocketAddr = if server.is_ipv4() {
        SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0)
    } else {
        SocketAddr::new(IpAddr::from([0u16; 8]), 0)
    };
    let socket = UdpSocket::bind(bind).map_err(|error| format!("bind UDP socket: {error}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("set DNS timeout: {error}"))?;
    socket
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("set DNS timeout: {error}"))?;
    socket
        .connect(server)
        .map_err(|error| format!("connect to {server}: {error}"))?;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() ^ u32::from(std::process::id() as u16))
        .unwrap_or(0x5eed);
    let id = (seed & 0xffff) as u16;
    let query = build_query(id, name)?;
    socket
        .send(&query)
        .map_err(|error| format!("send DNS query: {error}"))?;
    let mut buffer = vec![0u8; 4096];
    let received = socket
        .recv(&mut buffer)
        .map_err(|error| format!("receive DNS answer: {error}"))?;
    buffer.truncate(received);
    parse_txt_answer(&buffer, id)
}

fn build_query(id: u16, name: &str) -> Result<Vec<u8>, String> {
    let mut query = Vec::new();
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    query.extend_from_slice(&1u16.to_be_bytes());
    query.extend_from_slice(&0u16.to_be_bytes());
    query.extend_from_slice(&0u16.to_be_bytes());
    query.extend_from_slice(&0u16.to_be_bytes());
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("invalid DNS name `{name}`"));
        }
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.push(0);
    query.extend_from_slice(&16u16.to_be_bytes()); // TXT
    query.extend_from_slice(&1u16.to_be_bytes()); // IN
    Ok(query)
}

fn read_u16(packet: &[u8], offset: usize) -> Result<u16, String> {
    packet
        .get(offset..offset + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| "truncated DNS packet".to_string())
}

fn skip_name(packet: &[u8], mut offset: usize) -> Result<usize, String> {
    loop {
        let length = *packet
            .get(offset)
            .ok_or_else(|| "truncated DNS name".to_string())? as usize;
        if length == 0 {
            return Ok(offset + 1);
        }
        if length & 0xc0 == 0xc0 {
            return Ok(offset + 2);
        }
        offset = offset
            .checked_add(length + 1)
            .filter(|next| *next <= packet.len())
            .ok_or_else(|| "DNS name overruns the packet".to_string())?;
    }
}

fn parse_txt_answer(packet: &[u8], expected_id: u16) -> Result<Vec<String>, String> {
    if packet.len() < 12 {
        return Err("DNS answer is shorter than a header".to_string());
    }
    if read_u16(packet, 0)? != expected_id {
        return Err("DNS answer id does not match the query".to_string());
    }
    let flags = read_u16(packet, 2)?;
    let rcode = flags & 0x000f;
    if rcode == 3 {
        return Ok(Vec::new()); // NXDOMAIN: no key record exists.
    }
    if rcode != 0 {
        return Err(format!("DNS server returned rcode {rcode}"));
    }
    let questions = read_u16(packet, 4)? as usize;
    let answers = read_u16(packet, 6)? as usize;
    let mut offset = 12usize;
    for _ in 0..questions {
        offset = skip_name(packet, offset)?
            .checked_add(4)
            .ok_or("overflow")?;
    }
    let mut records = Vec::new();
    for _ in 0..answers {
        offset = skip_name(packet, offset)?;
        let record_type = read_u16(packet, offset)?;
        let data_len = read_u16(packet, offset + 8)? as usize;
        let data_start = offset + 10;
        let data_end = data_start
            .checked_add(data_len)
            .filter(|end| *end <= packet.len())
            .ok_or_else(|| "DNS answer overruns the packet".to_string())?;
        if record_type == 16 {
            let mut record = Vec::new();
            let mut cursor = data_start;
            while cursor < data_end {
                let chunk = packet[cursor] as usize;
                cursor += 1;
                let chunk_end = cursor
                    .checked_add(chunk)
                    .filter(|end| *end <= data_end)
                    .ok_or_else(|| "TXT string overruns its record".to_string())?;
                record.extend_from_slice(&packet[cursor..chunk_end]);
                cursor = chunk_end;
            }
            records.push(String::from_utf8_lossy(&record).to_string());
        }
        offset = data_end;
    }
    Ok(records)
}
