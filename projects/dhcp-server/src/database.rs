use std::net::Ipv4Addr;

use log::info;
use pnet::util::MacAddr;
use rusqlite::{params, Connection, Rows, NO_PARAMS};

pub fn select_addresses(
    con: &Connection,
    deleted: Option<u8>,
) -> Result<Vec<Ipv4Addr>, failure::Error> {
    if let Some(deleted) = deleted {
        let mut statement = con.prepare("SELECT ip_addr FROM lease_entries WHERE deleted = ?")?;
        let ip_addrs = statement.query(params![deleted.to_string()])?;
        get_addresses_from_row(ip_addrs)
    } else {
        let mut statement = con.prepare("SELECT ip_addr FROM lease_entries")?;
        let ip_addrs = statement.query(NO_PARAMS)?;
        get_addresses_from_row(ip_addrs)
    }
}

fn get_addresses_from_row(mut ip_addrs: Rows) -> Result<Vec<Ipv4Addr>, failure::Error> {
    let mut leased_addrs: Vec<Ipv4Addr> = Vec::new();
    while let Some(entry) = ip_addrs.next()? {
        let ip_addr = match entry.get(0) {
            Ok(ip) => {
                let ip: String = ip;
                ip.parse()?
            }
            Err(_) => continue,
        };
        leased_addrs.push(ip_addr);
    }
    Ok(leased_addrs)
}

pub fn select_entry(con: &Connection, mac_addr: MacAddr) -> Result<Option<Ipv4Addr>, failure::Error> {
    let mut stmt = con.prepare("SELECT ip_addr FROM lease_entries WHERE mac_addr = ?1")?;
    let mut row = stmt.query(params![mac_addr.to_string()])?;
    if let Some(entry) = row.next()? {
        let ip = entry.get(0)?;
        let ip_string: String = ip;
        Ok(Some(ip_string.parse()?))
    } else {
        info!("Specified MAC addr was not found.");
        Ok(None)
    }
}