extern crate cosmogony;
extern crate env_logger;
extern crate failure;
#[macro_use]
extern crate log;
extern crate serde_json;
extern crate structopt;
#[macro_use]
extern crate structopt_derive;
extern crate flate2;

use cosmogony::build_cosmogony;
use cosmogony::cosmogony::Cosmogony;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::prelude::*;
use structopt::StructOpt;

use failure::Error;

#[derive(StructOpt, Debug)]
struct Args {
    /// OSM PBF file.
    #[structopt(short = "i", long = "input")]
    input: String,
    /// output file name
    #[structopt(
        short = "o",
        long = "output",
        default_value = "cosmogony.json",
        help = "Output file name. Accepted formats are '.json' and '.json.gz'"
    )]
    output: Option<String>,
    #[structopt(help = "Do not display the stats", long = "no-stats")]
    no_stats: bool,
    #[structopt(help = "Do not read the geometry of the boundaries", long = "disable-geom")]
    disable_geom: bool,
    #[structopt(
        help = "country code if the pbf file does not contains any country", long = "country-code"
    )]
    country_code: Option<String>,
    #[structopt(
        help = "libpostal path",
        long = "libpostal",
        short = "l",
        default_value = "./libpostal/resources/boundaries/osm/"
    )]
    libpostal_path: String,
}

#[derive(PartialEq)]
enum OutputFormat {
    Json,
    JsonGz,
}

impl OutputFormat {
    fn all() -> Vec<OutputFormat> {
        vec![OutputFormat::Json, OutputFormat::JsonGz]
    }

    fn get_extension(&self) -> &str {
        match self {
            &OutputFormat::Json => ".json",
            &OutputFormat::JsonGz => ".json.gz",
        }
    }

    fn from_filename(filename: &str) -> Result<OutputFormat, Error> {
        for f in OutputFormat::all() {
            if filename.ends_with(f.get_extension()) {
                return Ok(f);
            }
        }
        Err(failure::err_msg(format!(
            "Unknown format in '{}'",
            filename
        )))
    }
}

fn serialize_cosmogony(
    cosmogony: &Cosmogony,
    output_file: String,
    format: OutputFormat,
) -> Result<(), Error> {
    let json = serde_json::to_string(cosmogony)?;
    let output_bytes = match format {
        OutputFormat::JsonGz => {
            let mut e = GzEncoder::new(vec![], Compression::default());
            e.write_all(json.as_bytes())?;
            e.finish()?
        }
        OutputFormat::Json => json.into_bytes(),
    };
    let mut file = File::create(output_file)?;
    file.write_all(&output_bytes)?;
    Ok(())
}

fn cosmogony(args: Args) -> Result<(), Error> {
    let format = if let Some(ref output_filename) = args.output {
        OutputFormat::from_filename(&output_filename)?
    } else {
        OutputFormat::Json
    };

    let cosmogony = build_cosmogony(
        args.input,
        !args.disable_geom,
        args.libpostal_path.into(),
        args.country_code,
    )?;

    if let Some(output) = args.output {
        serialize_cosmogony(&cosmogony, output, format)?;
    }

    if !args.no_stats {
        println!(
            "Statistics for {}:\n{}",
            cosmogony.meta.osm_filename, cosmogony.meta.stats
        );
    }
    Ok(())
}

fn main() {
    env_logger::init();
    let args = Args::from_args();
    match cosmogony(args) {
        Err(e) => {
            error!("cosmogony in error! {:?}", e);
            e.causes().for_each(|c| {
                error!("{}", c);
                if let Some(b) = c.backtrace() {
                    error!("  - {}", b);
                }
            });

            std::process::exit(1);
        }
        _ => (),
    }
}
