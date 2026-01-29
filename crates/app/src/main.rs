use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use cpal::traits::{DeviceTrait, HostTrait};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod audio;
mod autostart;
mod config;
mod daemon;
#[cfg(feature = "gui")]
mod gui;
mod pulse_info;
mod updater;
mod virtual_device;

#[derive(Parser)]
#[command(name = "voidmic")]
#[command(about = "VoidMic: Hybrid AI noise reduction", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// List available audio devices
    List,
    /// Run VoidMic in foreground (press Ctrl+C to stop)
    Run {
        #[arg(short, long, default_value = "default")]
        input: String,
        #[arg(short, long, default_value = "default")]
        output: String,
    },
    /// Load VoidMic: create virtual sink and start processing (daemonize)
    Load {
        #[arg(short, long, default_value = "default")]
        input: String,
    },
    /// Unload VoidMic: destroy virtual sink
    Unload,
    #[cfg(feature = "gui")]
    /// Launch the graphical interface
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
            let _engine = audio::AudioEngine::start(
                &input,
                &output,
                &model_path,
                0.015,
                1.0,
                false,
                None,
                false,
                2,               // Default VAD sensitivity (Aggressive)
                false,           // Default EQ disabled
                (0.0, 0.0, 0.0), // Default EQ gains
                false,           // AGC Disabled for CLI
                0.7,             // AGC Target
                false,           // Bypass Disabled
                None,            // No spectrum visualizer in CLI mode
            )?;
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
        Some(Commands::Load { input }) => {
            // NoiseTorch-like workflow: create virtual sink, start processing, daemonize
            #[cfg(target_os = "linux")]
            {
                use std::process::Command;

                // Create virtual sink
                match virtual_device::create_virtual_sink() {
                    Ok(device) => {
                        println!(
                            "✓ Virtual sink '{}' created",
                            virtual_device::VIRTUAL_SINK_NAME
                        );

                        // Get the monitor source name (this is what apps should use as input)
                        let monitor = virtual_device::get_monitor_source_name();

                        // Spawn background process
                        let exe = std::env::current_exe()?;
                        let output_sink = virtual_device::VIRTUAL_SINK_NAME.to_string();

                        let child = Command::new(&exe)
                            .args(["run", "-i", &input, "-o", &output_sink])
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();

                        match child {
                            Ok(c) => {
                                // Write PID file for the child process
                                let child_pid = c.id();
                                if let Err(e) = daemon::write_pid_file() {
                                    eprintln!("Warning: Could not write PID file: {}", e);
                                }
                                println!("✓ VoidMic started in background (PID: {})", child_pid);
                                println!(
                                    "\n📢 Select '{}' as your microphone in applications",
                                    monitor
                                );
                                println!("\nTo stop: voidmic unload");
                            }
                            Err(e) => {
                                eprintln!("Failed to start background process: {}", e);
                                // Cleanup sink
                                let _ = virtual_device::destroy_virtual_sink(device.module_id);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to create virtual sink: {}", e);
                        return Err(anyhow!("Virtual sink creation failed"));
                    }
                }
            }

            #[cfg(not(target_os = "linux"))]
            {
                let _ = input;
                println!("Load mode is only supported on Linux.");
                println!("Use 'voidmic run' on other platforms.");
            }
        }
        Some(Commands::Unload) => {
            #[cfg(target_os = "linux")]
            {
                // Try graceful shutdown using PID file first
                match daemon::stop_daemon() {
                    Ok(_) => println!("✓ Daemon stopped gracefully"),
                    Err(_) => {
                        // Fallback: Kill any running voidmic processes
                        let _ = std::process::Command::new("pkill")
                            .args(["-f", "voidmic run"])
                            .output();
                    }
                }

                // Destroy virtual sink
                match virtual_device::destroy_virtual_sink(0) {
                    Ok(_) => println!("✓ VoidMic unloaded"),
                    Err(e) => eprintln!("Warning: {}", e),
                }
            }

            #[cfg(not(target_os = "linux"))]
            {
                println!("Unload mode is only supported on Linux.");
            }
        }
        #[cfg(feature = "gui")]
        Some(Commands::Gui) => {
            gui::run_gui(model_path).map_err(|e| anyhow!("GUI Error: {}", e))?;
        }
        #[cfg(feature = "gui")]
        None => {
            gui::run_gui(model_path).map_err(|e| anyhow!("GUI Error: {}", e))?;
        }
        #[cfg(not(feature = "gui"))]
        None => {
            println!("GUI not available. Use 'voidmic run' for headless mode.");
            println!("Compile with --features gui for GUI support.");
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
