use std::net::UdpSocket;
use std::str;

pub fn serve(address: &str) -> Result<(), failure::Error> {
    let socket = UdpSocket::bind(address)?;
    loop {
        let mut buf = [0u8; 1024];
        debug!("Waiting for next data...");
        let (size, src) = socket.recv_from(&mut buf)?;
        debug!("Handling data from {} with size {}", src, size);
        println!("{}", str::from_utf8(&buf[..size])?);
        socket.send_to(&buf, src)?;
    }
}
