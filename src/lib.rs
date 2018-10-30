#[macro_use]
extern crate failure;
extern crate geo;
extern crate gst;
#[macro_use]
extern crate log;
extern crate ordered_float;
extern crate osm_boundaries_utils;
extern crate osmpbfreader;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate regex;
extern crate serde_yaml;
extern crate structopt;
#[macro_use]
extern crate lazy_static;
extern crate rayon;

mod additional_zones;
pub mod cosmogony;
mod country_finder;
mod hierarchy_builder;
mod mutable_slice;
mod utils;
pub mod zone;
mod zone_tree;
pub mod zone_typer;

use additional_zones::compute_additional_cities;
pub use cosmogony::{Cosmogony, CosmogonyMetadata, CosmogonyStats};
use country_finder::CountryFinder;
use failure::Error;
use failure::ResultExt;
use hierarchy_builder::{build_hierarchy, find_inclusions};
use mutable_slice::MutableSlice;
use osmpbfreader::{OsmId, OsmObj, OsmPbfReader};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

pub use zone::{Zone, ZoneIndex, ZoneType};

#[cfg_attr(rustfmt, rustfmt_skip)]
pub fn is_admin(obj: &OsmObj) -> bool {
    match *obj {
        OsmObj::Relation(ref rel) => {
            rel.tags
                .get("boundary")
                .map_or(false, |v| v == "administrative")
            &&
            rel.tags.get("admin_level").is_some()
        }
        _ => false,
    }
}

pub fn get_zones_and_stats(
    pbf: &mut OsmPbfReader<File>,
) -> Result<(Vec<zone::Zone>, CosmogonyStats), Error> {
    info!("Reading pbf with geometries...");
    let objects = pbf
        .get_objs_and_deps(|o| is_admin(o))
        .context("invalid osm file")?;
    info!("reading pbf done.");

    let mut zones = vec![];
    let stats = CosmogonyStats::default();

    for obj in objects.values() {
        if !is_admin(obj) {
            continue;
        }
        if let OsmObj::Relation(ref relation) = *obj {
            let next_index = ZoneIndex { index: zones.len() };
            if let Some(zone) = zone::Zone::from_osm_with_geom(relation, &objects, next_index) {
                // Ignore zone without boundary polygon for the moment
                if zone.boundary.is_some() {
                    zones.push(zone);
                }
            };
        }
    }

    return Ok((zones, stats));
}

pub fn get_zones_and_stats_without_geom(
    pbf: &mut OsmPbfReader<File>,
) -> Result<(Vec<zone::Zone>, CosmogonyStats), Error> {
    info!("Reading pbf without geometries...");

    let mut zones = vec![];
    let stats = CosmogonyStats::default();

    for obj in pbf.par_iter().map(Result::unwrap) {
        if !is_admin(&obj) {
            continue;
        }
        if let OsmObj::Relation(ref relation) = obj {
            let osm_id = OsmId::Relation(relation.id);
            let next_index = ZoneIndex { index: zones.len() };
            if let Some(zone) = zone::Zone::from_osm(&relation.tags, next_index, osm_id) {
                zones.push(zone);
            }
        }
    }

    Ok((zones, stats))
}

fn get_country_code<'a>(
    country_finder: &'a CountryFinder,
    zone: &zone::Zone,
    country_code: &'a Option<String>,
    inclusions: &Vec<ZoneIndex>,
) -> Option<String> {
    if let &Some(ref c) = country_code {
        Some(c.to_uppercase())
    } else {
        country_finder.find_zone_country(&zone, &inclusions)
    }
}

fn type_zones(
    zones: &mut [zone::Zone],
    stats: &mut CosmogonyStats,
    libpostal_file_path: PathBuf,
    country_code: Option<String>,
    inclusions: &Vec<Vec<ZoneIndex>>,
) -> Result<(), Error> {
    use rayon::prelude::*;
    info!("reading libpostal's rules");
    let zone_typer = zone_typer::ZoneTyper::new(libpostal_file_path)?;

    info!("creating a countrys rtree");
    let country_finder: CountryFinder = CountryFinder::init(&zones, &zone_typer);
    if country_code.is_none() && country_finder.is_empty() {
        return Err(failure::err_msg(
            "no country_code has been provided and no country have been found, we won't be able to make a cosmogony",
        ));
    }

    info!("typing zones");
    // We type all the zones in parallele
    // To not mutate the zones while doing it
    // (the borrow checker would not be happy since we also need to access to the zone's vector
    // to be able to transform the ZoneIndex to a zone)
    // we collect all the types in a Vector, and assign the zone's zone_type as a post process
    let zones_type: Vec<_> = zones
        .par_iter()
        .map(|z| {
            get_country_code(&country_finder, &z, &country_code, &inclusions[z.id.index])
                .map(|c| zone_typer.get_zone_type(&z, &c, &inclusions[z.id.index], zones))
        }).collect();

    zones
        .iter_mut()
        .zip(zones_type.into_iter())
        .for_each(|(z, zone_type)| match zone_type {
            None => {
                info!(
                    "impossible to find a country for {} ({}), skipping",
                    z.osm_id, z.name
                );
                stats.zone_without_country += 1;
            }
            Some(Ok(t)) => z.zone_type = Some(t),
            Some(Err(zone_typer::ZoneTyperError::InvalidCountry(c))) => {
                info!("impossible to find rules for country {}", c);
                *stats.zone_with_unkwown_country_rules.entry(c).or_insert(0) += 1;
            }
            Some(Err(zone_typer::ZoneTyperError::UnkownLevel(lvl, country))) => {
                debug!(
                    "impossible to find a rule for level {:?} for country {}",
                    lvl, country
                );
                *stats
                    .unhandled_admin_level
                    .entry(country)
                    .or_insert(BTreeMap::new())
                    .entry(lvl.unwrap_or(0))
                    .or_insert(0) += 1;
            }
        });

    Ok(())
}

fn compute_labels(zones: &mut [Zone]) {
    info!("computing all zones's label");
    let nb_zones = zones.len();
    for i in 0..nb_zones {
        let (mslice, z) = MutableSlice::init(zones, i);
        z.compute_labels(&mslice);
    }
}

// we don't want to keep zone's without zone_type (but the zone_type could be ZoneType::NonAdministrative)
fn clean_untagged_zones(zones: &mut Vec<zone::Zone>) {
    info!("cleaning untagged zones");
    let nb_zones = zones.len();
    zones.retain(|z| z.zone_type.is_some());
    info!("{} zones cleaned", (nb_zones - zones.len()));
}

fn create_ontology(
    zones: &mut Vec<zone::Zone>,
    stats: &mut CosmogonyStats,
    libpostal_file_path: PathBuf,
    country_code: Option<String>,
    pbf_reader: &mut OsmPbfReader<File>,
) -> Result<(), Error> {
    info!("creating ontology for {} zones", zones.len());
    let inclusions = find_inclusions(zones);

    type_zones(zones, stats, libpostal_file_path, country_code, &inclusions)?;

    build_hierarchy(zones, inclusions);

    compute_additional_cities(zones, pbf_reader);

    compute_labels(zones);

    // we remove the useless zones from cosmogony
    // WARNING: this invalidate the different indexes  (we can no longer lookup a Zone by it's id in the zones's vector)
    // this should be removed later on (and switch to a map by osm_id ?) as it's not elegant,
    // but for the moment it'll do
    clean_untagged_zones(zones);

    Ok(())
}

pub fn build_cosmogony(
    pbf_path: String,
    with_geom: bool,
    libpostal_file_path: PathBuf,
    country_code: Option<String>,
) -> Result<Cosmogony, Error> {
    let path = Path::new(&pbf_path);
    let file = File::open(&path).context("no pbf file")?;

    let mut parsed_pbf = OsmPbfReader::new(file);

    let (mut zones, mut stats) = if with_geom {
        get_zones_and_stats(&mut parsed_pbf)?
    } else {
        get_zones_and_stats_without_geom(&mut parsed_pbf)?
    };

    create_ontology(
        &mut zones,
        &mut stats,
        libpostal_file_path,
        country_code,
        &mut parsed_pbf,
    )?;

    stats.compute(&zones);

    let cosmogony = Cosmogony {
        zones: zones,
        meta: CosmogonyMetadata {
            osm_filename: path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|f| f.to_string())
                .unwrap_or("invalid file name".into()),
            stats: stats,
        },
    };
    Ok(cosmogony)
}
