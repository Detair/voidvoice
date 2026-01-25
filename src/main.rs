use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use cpal::traits::{DeviceTrait, HostTrait};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod audio;
mod gui;
mod config;

#[derive(Parser)]
#[command(name = "voidmic")]
#[command(about = "VoidMic: Hybrid AI noise reduction", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    List,
    Run {
        #[arg(short, long, default_value = "default")]
        input: String,
        #[arg(short, long, default_value = "default")]
        output: String,
    },
    Gui,
}

fn main() -> Result<()> {
    env_logger::init();
    
    // RNNoise weights are embedded in the nnnoiseless crate
    let model_path = PathBuf::from("Embedded"); 

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::List) => {
            list_devices()?;
        }
        Some(Commands::Run { input, output }) => {
            let _engine = audio::AudioEngine::start(&input, &output, &model_path)?;
            println!("VoidMic Active (Hybrid). Press Ctrl+C to stop.");
            
            // Graceful shutdown handling
            let running = Arc::new(AtomicBool::new(true));
            let r = running.clone();
            
            ctrlc::set_handler(move || {
                println!("\nShutting down gracefully...");
                r.store(false, Ordering::Relaxed);
            })?;
            
            while running.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            
            println!("VoidMic stopped.");
        }
        Some(Commands::Gui) | None => {
            gui::run_gui(model_path).map_err(|e| anyhow!("GUI Error: {}", e))?;
        }
    }

    Ok(())
}

fn list_devices() -> Result<()> {
    let host = cpal::default_host();
    println!("Audio Host: {}", host.id().name());
    println!("\nInput Devices:");
    for device in host.input_devices()? {
        println!("  - {}", device.name().unwrap_or("Unknown".to_string()));
    }
    println!("\nOutput Devices:");
    for device in host.output_devices()? {
        println!("  - {}", device.name().unwrap_or("Unknown".to_string()));
    }
    Ok(())
}
