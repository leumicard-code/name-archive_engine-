use anyhow::{bail, Context, Result};
use archive_engine::{compress, extract, format_size, list_archive, Format};
use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use rfd::FileDialog;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "archive-engine", version = "0.3.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    List {
        archive: Option<PathBuf>,
    },
    Compress {
        inputs: Vec<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long, value_enum, default_value_t = CliFormat::TarZstd)]
        format: CliFormat,
        #[arg(short, long, default_value_t = 5)]
        level: i32,
        #[arg(long)]
        pick: bool,
    },
    Extract {
        archive: Option<PathBuf>,
        #[arg(short, long)]
        dest: Option<PathBuf>,
        #[arg(long)]
        pick: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum CliFormat {
    Zip,
    Tar,
    TarGz,
    TarZstd,
    Zstd,
    SevenZ,
    TarXz,
    Xz,
}

impl From<CliFormat> for Format {
    fn from(f: CliFormat) -> Self {
        match f {
            CliFormat::Zip => Format::Zip,
            CliFormat::Tar => Format::Tar,
            CliFormat::TarGz => Format::TarGz,
            CliFormat::TarZstd => Format::TarZstd,
            CliFormat::Zstd => Format::Zstd,
            CliFormat::SevenZ => Format::SevenZ,
            CliFormat::TarXz => Format::TarXz,
            CliFormat::Xz => Format::Xz,
        }
    }
}

fn make_progress(len: u64, msg: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List { archive } => {
            let path = match archive {
                Some(p) => p,
                None => FileDialog::new()
                    .add_filter(
                        "Archives",
                        &["zip", "tar", "gz", "tgz", "zst", "zstd", "7z", "xz", "txz"],
                    )
                    .pick_file()
                    .context("לא נבחר קובץ")?,
            };
            let entries = list_archive(&path)?;
            println!("{:<55} {:>12} {:>12}  {}", "שם", "גודל", "דחוס", "סוג");
            println!("{}", "─".repeat(95));
            for e in entries {
                let kind = if e.is_dir { "DIR" } else { "FILE" };
                let comp = e.compressed.map(format_size).unwrap_or_else(|| "-".into());
                println!(
                    "{:<55} {:>12} {:>12}  {}",
                    e.path,
                    format_size(e.size),
                    comp,
                    kind
                );
            }
        }

        Commands::Compress {
            mut inputs,
            output,
            format,
            level,
            pick,
        } => {
            if pick || inputs.is_empty() {
                if let Some(files) = FileDialog::new().pick_files() {
                    inputs.extend(files);
                }
                if let Some(folder) = FileDialog::new().pick_folder() {
                    inputs.push(folder);
                }
                if inputs.is_empty() {
                    bail!("לא נבחרו קבצים");
                }
            }

            let engine_fmt: Format = format.into();
            let output = match output {
                Some(p) => p,
                None => {
                    let ext = engine_fmt.extension();
                    FileDialog::new()
                        .add_filter("Archive", &[ext])
                        .set_file_name(&format!("archive.{}", ext))
                        .save_file()
                        .context("לא נבחר מיקום שמירה")?
                }
            };

            let pb = make_progress(inputs.len() as u64, "מכווץ");
            let refs: Vec<&Path> = inputs.iter().map(|p| p.as_path()).collect();
            compress(&refs, &output, engine_fmt, level, |curr, total, name| {
                pb.set_length(total as u64);
                pb.set_position(curr as u64);
                pb.set_message(name.to_string());
            })?;
            pb.finish_with_message("הושלם");
            println!("✓ נוצר בהצלחה: {}", output.display());
        }

        Commands::Extract {
            archive,
            dest,
            pick,
        } => {
            let archive = match archive {
                Some(p) => p,
                None if pick || true => FileDialog::new()
                    .add_filter(
                        "Archives",
                        &["zip", "tar", "gz", "tgz", "zst", "zstd", "7z", "xz", "txz"],
                    )
                    .pick_file()
                    .context("לא נבחר ארכיון")?,
                None => bail!("יש לציין ארכיון"),
            };

            let dest = match dest {
                Some(p) => p,
                None => FileDialog::new()
                    .pick_folder()
                    .context("לא נבחרה תיקיית יעד")?,
            };

            extract(&archive, &dest)?;
            println!("✓ נפרס בהצלחה אל: {}", dest.display());
        }
    }

    Ok(())
}
