use actix_web::dev::{ConnectionInfo, Params};
use actix_web::http::header::HeaderMap;
use serde_json;
use std::collections::HashMap;
use tilejson::{TileJSON, TileJSONBuilder};

use super::source::{Source, XYZ};

pub fn build_tilejson(
  source: Box<dyn Source>,
  connection_info: ConnectionInfo,
  headers: HeaderMap,
) -> TileJSON {
  let source_id = source.get_id();

  let path = headers
    .get("x-rewrite-url")
    .map_or(String::from(source_id), |header| {
      let parts: Vec<&str> = header.to_str().unwrap().split('.').collect();
      let (_, parts_without_extension) = parts.split_last().unwrap();
      let path_without_extension = parts_without_extension.join(".");
      let (_, path_without_leading_slash) = path_without_extension.split_at(1);

      String::from(path_without_leading_slash)
    });

  let tiles_url = format!(
    "{}://{}/{}/{{z}}/{{x}}/{{y}}.pbf",
    connection_info.scheme(),
    connection_info.host(),
    path
  );

  let mut tilejson_builder = TileJSONBuilder::new();
  tilejson_builder.scheme("tms");
  tilejson_builder.name(source_id);
  tilejson_builder.tiles(vec![&tiles_url]);

  tilejson_builder.finalize()
}

pub fn parse_xyz(params: &Params) -> Result<XYZ, &str> {
  let z = params
    .get("z")
    .and_then(|i| i.parse::<u32>().ok())
    .ok_or("invalid z value")?;

  let x = params
    .get("x")
    .and_then(|i| i.parse::<u32>().ok())
    .ok_or("invalid x value")?;

  let y = params
    .get("y")
    .and_then(|i| i.parse::<u32>().ok())
    .ok_or("invalid y value")?;

  Ok(XYZ { x, y, z })
}

// https://github.com/mapbox/postgis-vt-util/blob/master/src/TileBBox.sql
pub fn tilebbox(xyz: XYZ) -> String {
  let x = xyz.x;
  let y = xyz.y;
  let z = xyz.z;

  let max = 20037508.34;
  let res = (max * 2.0) / (2_i32.pow(z) as f64);

  let xmin = -max + (x as f64 * res);
  let ymin = max - (y as f64 * res);
  let xmax = -max + (x as f64 * res) + res;
  let ymax = max - (y as f64 * res) - res;

  format!(
    "ST_MakeEnvelope({0}, {1}, {2}, {3}, 3857)",
    xmin, ymin, xmax, ymax
  )
}

pub fn json_to_hashmap(value: serde_json::Value) -> HashMap<String, String> {
  let mut hashmap = HashMap::new();

  let object = value.as_object().unwrap();
  for (key, value) in object {
    let string_value = value.as_str().unwrap();
    hashmap.insert(key.to_string(), string_value.to_string());
  }

  hashmap
}