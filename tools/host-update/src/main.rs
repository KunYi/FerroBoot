//! Host Update Serial Transport CLI
//!
//! Sends signed & encrypted OTA images over USART1 to Bootloader rescue mode
//! or Application update listener.

use clap::Parser;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(author, version, about = "Host Serial Firmware Updater", long_about = None)]
struct Args {
    #[arg(short, long)]
    port: String,

    #[arg(short, long, default_value_t = 115200)]
    baud: u32,

    #[arg(short, long)]
    image: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!("[host-update] Connecting to {} at {} baud...", args.port, args.baud);

    let mut port = serialport::new(&args.port, args.baud)
        .timeout(Duration::from_millis(1000))
        .open()?;

    let data = fs::read(&args.image)?;
    println!("[host-update] Sending firmware image ({:?}, {} bytes)...", args.image, data.len());

    let chunk_size = 512;
    let mut offset = 0;

    while offset < data.len() {
        let end = (offset + chunk_size).min(data.len());
        let chunk = &data[offset..end];
        port.write_all(chunk)?;
        offset = end;
        print!("\r[host-update] Progress: {} / {} bytes ({:.1}%)",
            offset, data.len(), (offset as f64 / data.len() as f64) * 100.0);
    }

    println!("\r\n[host-update] Image upload finished successfully.");
    Ok(())
}
