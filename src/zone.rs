extern crate geo;
extern crate geojson;
extern crate geos;
extern crate itertools;
extern crate serde;
extern crate serde_json;

use self::geos::GGeom;
use self::itertools::Itertools;
use self::serde::Serialize;
use geo::Point;
use mutable_slice::MutableSlice;
use osm_boundaries_utils::build_boundary;
use osmpbfreader::objects::{OsmId, OsmObj, Relation, Tags};
use std;
use std::collections::BTreeMap;
use std::fmt;
use zone::geos::from_geo::TryInto;

type Coord = Point<f64>;

#[derive(Serialize, Deserialize, Copy, Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ZoneType {
    Suburb,
    CityDistrict,
    City,
    StateDistrict,
    State,
    CountryRegion,
    Country,
    NonAdministrative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneIndex {
    pub index: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Zone {
    pub id: ZoneIndex,
    pub osm_id: String,
    pub admin_level: Option<u32>,
    pub zone_type: Option<ZoneType>,
    pub name: String,
    #[serde(default)]
    pub label: String,
    pub zip_codes: Vec<String>,
    #[serde(serialize_with = "serialize_as_geojson", deserialize_with = "deserialize_as_coord")]
    pub center: Option<Coord>,
    #[serde(
        serialize_with = "serialize_as_geojson",
        deserialize_with = "deserialize_as_multipolygon",
        rename = "geometry",
        default
    )]
    pub boundary: Option<geo::MultiPolygon<f64>>,

    pub tags: Tags,

    pub parent: Option<ZoneIndex>,
    pub wikidata: Option<String>,
    // pub links: Vec<ZoneIndex>
}

impl Zone {
    pub fn is_admin(&self) -> bool {
        match self.zone_type {
            None => false,
            Some(ZoneType::NonAdministrative) => false,
            _ => true,
        }
    }

    pub fn set_parent(&mut self, idx: Option<ZoneIndex>) {
        self.parent = idx;
    }

    pub fn from_osm(relation: &Relation, index: ZoneIndex) -> Option<Self> {
        // Skip administrative region without name
        let name = match relation.tags.get("name") {
            Some(val) => val,
            None => {
                warn!(
                    "relation/{}: adminstrative region without name, skipped",
                    relation.id.0
                );
                return None;
            }
        };
        let level = relation
            .tags
            .get("admin_level")
            .and_then(|s| s.parse().ok());

        let zip_code = relation
            .tags
            .get("addr:postcode")
            .or_else(|| relation.tags.get("postal_code"))
            .map_or("", |val| &val[..]);
        let zip_codes = zip_code
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .sorted();
        let wikidata = relation.tags.get("wikidata").map(|s| s.to_string());

        Some(Self {
            id: index,
            osm_id: format!("relation:{}", relation.id.0.to_string()), // for the moment we can only read relation
            admin_level: level,
            zone_type: None,
            name: name.to_string(),
            label: "".to_string(),
            zip_codes: zip_codes,
            center: None,
            boundary: None,
            parent: None,
            tags: relation.tags.clone(),
            wikidata: wikidata,
        })
    }

    pub fn from_osm_with_geom(
        relation: &Relation,
        objects: &BTreeMap<OsmId, OsmObj>,
        index: ZoneIndex,
    ) -> Option<Self> {
        use geo::centroid::Centroid;
        Self::from_osm(relation, index).map(|mut result| {
            result.boundary = build_boundary(relation, objects);

            result.center = relation
                .refs
                .iter()
                .find(|r| r.role == "admin_centre")
                .and_then(|r| objects.get(&r.member))
                .and_then(|o| o.node())
                .map_or(
                    result.boundary.as_ref().and_then(|b| b.centroid()),
                    |node| Some(Coord::new(node.lon(), node.lat())),
                );

            result
        })
    }

    pub fn contains(&self, other: &Zone) -> bool {
        match (&self.boundary, &other.boundary) {
            (&Some(ref mpoly1), &Some(ref mpoly2)) => {
                let m_self: Result<GGeom, _> = mpoly1.try_into();
                let m_other: Result<GGeom, _> = mpoly2.try_into();

                match (&m_self, &m_other) {
                    (&Ok(ref m_self), &Ok(ref m_other)) => {
                        // In GEOS, "covers" is less strict than "contains".
                        // eg: a polygon does NOT "contain" its boundary, but "covers" it.
                        m_self.covers(&m_other)
                    }
                    (&Err(ref e), _) => {
                        info!(
                            "impossible to convert to geos for zone {:?}, error {}",
                            &self.osm_id, e
                        );
                        debug!(
                            "impossible to convert to geos the zone {:?}",
                            serde_json::to_string(&self)
                        );
                        false
                    }
                    (_, &Err(ref e)) => {
                        info!(
                            "impossible to convert to geos for zone {:?}, error {}",
                            &other.osm_id, e
                        );
                        debug!(
                            "impossible to convert to geos the zone {:?}",
                            serde_json::to_string(&other)
                        );
                        false
                    }
                }
            }
            _ => false,
        }
    }

    pub fn iter_parents<'a>(&'a self, all_zones: &'a MutableSlice) -> ParentIterator<'a> {
        ParentIterator {
            zone: &self,
            all_zones: all_zones,
        }
    }

    /// compute a nice human readable label
    /// The label carries the hierarchy of a zone.
    ///
    /// This label is inspired from
    /// [opencage formatting](https://blog.opencagedata.com/post/99059889253/good-looking-addresses-solving-the-berlin-berlin)
    ///
    /// and from the [mimirsbrunn](https://github.com/CanalTP/mimirsbrunn) zip code formatting
    ///
    /// example of zone's label:
    /// Paris (75000-75116), Île-de-France, France
    pub fn compute_label(&mut self, all_zones: &MutableSlice) {
        let mut hierarchy: Vec<_> = std::iter::once(self.name.clone())
            .chain(self.iter_parents(all_zones).map(|z| z.name.clone()))
            .dedup()
            .collect();

        if let Some(ref mut zone_name) = hierarchy.first_mut() {
            zone_name.push_str(&format_zip_code(&self.zip_codes));
        }
        let label = hierarchy.join(", ");

        self.label = label;
    }
}

/// format the zone's zip code
/// if no zipcode, we return an empty string
/// if only one zipcode, we return it between ()
/// if more than one we display the range of zips code
///
/// This way for example Paris will get " (75000-75116)"
///
/// ruthlessly taken from mimir
fn format_zip_code(zip_codes: &[String]) -> String {
    match zip_codes.len() {
        0 => "".to_string(),
        1 => format!(" ({})", zip_codes.first().unwrap()),
        _ => format!(
            " ({}-{})",
            zip_codes.first().unwrap_or(&"".to_string()),
            zip_codes.last().unwrap_or(&"".to_string())
        ),
    }
}

pub struct ParentIterator<'a> {
    zone: &'a Zone,
    all_zones: &'a MutableSlice<'a>,
}

impl<'a> Iterator for ParentIterator<'a> {
    type Item = &'a Zone;
    fn next(&mut self) -> Option<&'a Zone> {
        let p = &self.zone.parent;
        match p {
            &Some(ref z_idx) => {
                self.zone = self.all_zones.get(&z_idx);
                Some(self.zone)
            }
            &None => None,
        }
    }
}

// those 2 methods have been shamelessly copied from https://github.com/CanalTP/mimirsbrunn/blob/master/libs/mimir/src/objects.rs#L277
// see if there is a good way to share the code
fn serialize_as_geojson<'a, S, T>(
    multi_polygon_option: &'a Option<T>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    geojson::Value: From<&'a T>,
    S: serde::Serializer,
{
    use self::geojson::{GeoJson, Geometry, Value};
    use self::serde::Serialize;

    match *multi_polygon_option {
        Some(ref multi_polygon) => {
            GeoJson::Geometry(Geometry::new(Value::from(multi_polygon))).serialize(serializer)
        }
        None => serializer.serialize_none(),
    }
}

fn deserialize_geom<'de, D>(d: D) -> Result<Option<geo::Geometry<f64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use self::geojson;
    use self::geojson::conversion::TryInto;
    use self::serde::Deserialize;

    Option::<geojson::GeoJson>::deserialize(d).map(|option| {
        option.and_then(|geojson| match geojson {
            geojson::GeoJson::Geometry(geojson_geom) => {
                let geo_geom: Result<geo::Geometry<f64>, _> = geojson_geom.value.try_into();
                match geo_geom {
                    Ok(g) => Some(g),
                    Err(e) => {
                        warn!("Error deserializing geometry: {}", e);
                        None
                    }
                }
            }
            _ => None,
        })
    })
}

fn deserialize_as_multipolygon<'de, D>(d: D) -> Result<Option<geo::MultiPolygon<f64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match deserialize_geom(d)? {
        Some(geo::Geometry::MultiPolygon(geo_multi_polygon)) => Ok(Some(geo_multi_polygon)),
        None => Ok(None),
        Some(_) => Err(serde::de::Error::custom(
            "invalid geometry type, should be a multipolygon",
        )),
    }
}

fn deserialize_as_coord<'de, D>(d: D) -> Result<Option<Coord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match deserialize_geom(d)? {
        Some(geo::Geometry::Point(p)) => Ok(Some(p)),
        None => Ok(None),
        Some(_) => Err(serde::de::Error::custom(
            "invalid geometry type, should be a point",
        )),
    }
}

impl Serialize for ZoneIndex {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u64(self.index as u64)
    }
}

impl<'de> serde::Deserialize<'de> for ZoneIndex {
    fn deserialize<D>(deserializer: D) -> Result<ZoneIndex, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_u64(ZoneIndexVisitor)
    }
}

struct ZoneIndexVisitor;

impl<'de> serde::de::Visitor<'de> for ZoneIndexVisitor {
    type Value = ZoneIndex;

    fn visit_u64<E>(self, data: u64) -> Result<ZoneIndex, E>
    where
        E: serde::de::Error,
    {
        Ok(ZoneIndex {
            index: data as usize,
        })
    }

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a zone index")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    fn make_zone(name: &str, id: usize) -> Zone {
        make_zone_and_zip(name, id, vec![], None)
    }

    fn make_zone_and_zip(name: &str, id: usize, zips: Vec<&str>, parent: Option<usize>) -> Zone {
        Zone {
            id: ZoneIndex { index: id },
            osm_id: "".into(),
            admin_level: None,
            zone_type: Some(ZoneType::City),
            name: name.into(),
            label: "".into(),
            center: None,
            boundary: None,
            parent: parent.map(|p| ZoneIndex { index: p }),
            tags: Tags::new(),
            wikidata: None,
            zip_codes: zips.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn simple_label_test() {
        let mut zones = vec![make_zone("toto", 0)];

        let (mslice, z) = MutableSlice::init(&mut zones, 0);
        z.compute_label(&mslice);
        assert_eq!(z.label, "toto");
    }

    #[test]
    fn label_with_zip_and_parent() {
        let mut zones = vec![
            make_zone_and_zip("bob", 0, vec!["75020", "75021", "75022"], Some(1)),
            make_zone_and_zip("bob sur mer", 1, vec!["75"], Some(2)), // it's zip code shouldn't be used
            make_zone("bobette's land", 2),
        ];

        let (mslice, z) = MutableSlice::init(&mut zones, 0);
        z.compute_label(&mslice);
        assert_eq!(z.label, "bob (75020-75022), bob sur mer, bobette's land");
    }

    #[test]
    fn label_with_zip_and_double_parent() {
        // we should not have any double in the label
        let mut zones = vec![
            make_zone_and_zip("bob", 0, vec!["75020"], Some(1)),
            make_zone_and_zip("bob", 1, vec![], Some(2)),
            make_zone_and_zip("bob", 2, vec![], Some(3)),
            make_zone_and_zip("bob sur mer", 3, vec!["75"], Some(4)),
            make_zone_and_zip("bob sur mer", 4, vec!["75"], Some(5)),
            make_zone("bobette's land", 5),
        ];

        let (mslice, z) = MutableSlice::init(&mut zones, 0);
        z.compute_label(&mslice);
        assert_eq!(z.label, "bob (75020), bob sur mer, bobette's land");
    }

    #[test]
    fn label_with_zip_and_parent_named_as_zone() {
        // we should not have any consecutive double in the labl
        // but non consecutive double should not be cleaned
        let mut zones = vec![
            make_zone_and_zip("bob", 0, vec!["75020"], Some(1)),
            make_zone_and_zip("bob sur mer", 1, vec!["75"], Some(2)),
            make_zone("bob", 2),
        ];

        let (mslice, z) = MutableSlice::init(&mut zones, 0);
        z.compute_label(&mslice);
        assert_eq!(z.label, "bob (75020), bob sur mer, bob");
    }
}
