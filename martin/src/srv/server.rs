use std::future::Future;
use std::pin::Pin;
use std::string::ToString;
use std::time::Duration;

use crate::args::{Args, OsEnv};
use crate::config::ServerState;
use crate::source::TileCatalog;
use crate::srv::config::{SrvConfig, KEEP_ALIVE_DEFAULT, LISTEN_ADDRESSES_DEFAULT};
use crate::srv::tiles::get_tile;
use crate::srv::tiles_info::get_source_info;
use crate::utils::OptMainCache;
use crate::MartinError::BindingError;
use crate::{read_config, TileSources};
use crate::{Config, MartinResult};
use actix_cors::Cors;
use actix_web::error::ErrorInternalServerError;
use actix_web::http::header::CACHE_CONTROL;
use actix_web::middleware::TrailingSlash;
use actix_web::web::Data;
use actix_web::{middleware, route, web, App, HttpResponse, HttpServer, Responder};
use futures::TryFutureExt;
#[cfg(feature = "lambda")]
use lambda_web::{is_running_on_lambda, run_actix_on_lambda};
use log::{error, info};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// List of keywords that cannot be used as source IDs. Some of these are reserved for future use.
/// Reserved keywords must never end in a "dot number" (e.g. ".1").
/// This list is documented in the `docs/src/using.md` file, which should be kept in sync.
pub const RESERVED_KEYWORDS: &[&str] = &[
    "_", "catalog", "config", "font", "health", "help", "index", "manifest", "metrics", "refresh",
    "reload", "sprite", "status",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Catalog {
    pub tiles: TileCatalog,
    #[cfg(feature = "sprites")]
    pub sprites: crate::sprites::SpriteCatalog,
    #[cfg(feature = "fonts")]
    pub fonts: crate::fonts::FontCatalog,
}

impl Catalog {
    pub fn new(state: &ServerState) -> MartinResult<Self> {
        Ok(Self {
            tiles: state.tiles.get_catalog(),
            #[cfg(feature = "sprites")]
            sprites: state.sprites.get_catalog()?,
            #[cfg(feature = "fonts")]
            fonts: state.fonts.get_catalog(),
        })
    }
}

pub fn map_internal_error<T: std::fmt::Display>(e: T) -> actix_web::Error {
    error!("{e}");
    ErrorInternalServerError(e.to_string())
}

/// Root path will eventually have a web front. For now, just a stub.
#[route("/", method = "GET", method = "HEAD")]
#[allow(clippy::unused_async)]
async fn get_index() -> &'static str {
    // todo: once this becomes more substantial, add wrap = "middleware::Compress::default()"
    "Martin server is running. Eventually this will be a nice web front.\n\n\
    A list of all available sources is at /catalog\n\n\
    See documentation https://github.com/maplibre/martin"
}

/// Return 200 OK if healthy. Used for readiness and liveness probes.
#[route("/health", method = "GET", method = "HEAD")]
#[allow(clippy::unused_async)]
async fn get_health() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((CACHE_CONTROL, "no-cache"))
        .message_body("OK")
}

#[allow(clippy::too_many_arguments)]
#[route("/refresh", method = "POST")]
#[allow(clippy::unused_async)]
async fn refresh_catalog(
    args: Data<Args>,
    env: Data<OsEnv>,
    srv_config_guard: Data<RwLock<SrvConfig>>,
    catalog_guard: Data<RwLock<Catalog>>,
    state_guard: Data<RwLock<ServerState>>,
    tiles_guard: Data<RwLock<TileSources>>,
    cache_guard: Data<RwLock<OptMainCache>>,

    #[cfg(feature = "sprites")] sprites_guard: Data<RwLock<crate::sprites::SpriteSources>>,

    #[cfg(feature = "fonts")] fonts_guard: Data<RwLock<crate::fonts::FontSources>>,
) -> actix_web::error::Result<HttpResponse> {
    let mut config = if let Some(ref cfg_filename) = args.meta.config {
        info!("Using {} to refresh catalog", cfg_filename.display());
        read_config(cfg_filename, env.get_ref()).map_err(map_internal_error)?
    } else {
        info!("Config file is not specified, an default config will be used to refresh catalog");
        Config::default()
    };
    let cloned_args = (**args).clone();
    cloned_args
        .merge_into_config(&mut config, env.get_ref())
        .map_err(map_internal_error)?;

    config.finalize().map_err(map_internal_error)?;

    let sources = config.resolve().await.map_err(map_internal_error)?;

    // update these two guards
    let new_srv_config = config.srv;
    let new_state = sources;
    let new_catalog = Catalog::new(&new_state).map_err(map_internal_error)?;
    let new_tiles = new_state.tiles.clone();
    let new_cache = new_state.cache.clone();

    let mut srv_config = srv_config_guard.write().await;
    let mut state = state_guard.write().await;
    let mut catalog = catalog_guard.write().await;
    let mut tiles = tiles_guard.write().await;
    let mut cache = cache_guard.write().await;

    #[cfg(feature = "sprites")]
    {
        let mut sprites = sprites_guard.write().await;
        *sprites = new_state.sprites.clone();
    }
    #[cfg(feature = "fonts")]
    {
        let mut fonts = fonts_guard.write().await;
        *fonts = new_state.fonts.clone();
    }

    *srv_config = new_srv_config;
    *state = new_state;
    *catalog = new_catalog;
    *tiles = new_tiles;
    *cache = new_cache;

    Ok(HttpResponse::Ok().finish())
}

#[route(
    "/catalog",
    method = "GET",
    method = "HEAD",
    wrap = "middleware::Compress::default()"
)]
#[allow(clippy::unused_async)]
async fn get_catalog(catalog: Data<RwLock<Catalog>>) -> impl Responder {
    let catalog_guard = catalog.read().await;
    HttpResponse::Ok().json(&*catalog_guard)
}

pub fn router(cfg: &mut web::ServiceConfig) {
    cfg.service(get_health)
        .service(get_index)
        .service(get_catalog)
        .service(refresh_catalog)
        .service(get_source_info)
        .service(get_tile);

    #[cfg(feature = "sprites")]
    cfg.service(crate::srv::sprites::get_sprite_json)
        .service(crate::srv::sprites::get_sprite_png);

    #[cfg(feature = "fonts")]
    cfg.service(crate::srv::fonts::get_font);
}

type Server = Pin<Box<dyn Future<Output = MartinResult<()>>>>;

/// Create a future for an Actix web server together with the listening address.
pub fn new_server(
    env: OsEnv,
    args: Args,
    config: SrvConfig,
    state: ServerState,
) -> MartinResult<(Server, String)> {
    let catalog = Catalog::new(&state)?;
    let keep_alive = Duration::from_secs(config.keep_alive.unwrap_or(KEEP_ALIVE_DEFAULT));
    let worker_processes = config.worker_processes.unwrap_or_else(num_cpus::get);
    let listen_addresses = config
        .listen_addresses
        .clone()
        .unwrap_or_else(|| LISTEN_ADDRESSES_DEFAULT.to_string());

    let factory = move || {
        let cors_middleware = Cors::default()
            .allow_any_origin()
            .allowed_methods(vec!["GET"]);

        let app = App::new()
            .app_data(Data::new(RwLock::new(state.tiles.clone())))
            .app_data(Data::new(RwLock::new(state.cache.clone())))
            .app_data(Data::new(RwLock::new(state.clone())));

        #[cfg(feature = "sprites")]
        let app = app.app_data(Data::new(RwLock::new(state.sprites.clone())));

        #[cfg(feature = "fonts")]
        let app = app.app_data(Data::new(RwLock::new(state.fonts.clone())));

        app.app_data(Data::new(env.clone()))
            .app_data(Data::new(args.clone()))
            .app_data(Data::new(RwLock::new(catalog.clone())))
            .app_data(Data::new(RwLock::new(config.clone())))
            .wrap(cors_middleware)
            .wrap(middleware::NormalizePath::new(TrailingSlash::MergeOnly))
            .wrap(middleware::Logger::default())
            .configure(router)
    };

    #[cfg(feature = "lambda")]
    if is_running_on_lambda() {
        let server = run_actix_on_lambda(factory).err_into();
        return Ok((Box::pin(server), "(aws lambda)".into()));
    }

    let server = HttpServer::new(factory)
        .bind(listen_addresses.clone())
        .map_err(|e| BindingError(e, listen_addresses.clone()))?
        .keep_alive(keep_alive)
        .shutdown_timeout(0)
        .workers(worker_processes)
        .run()
        .err_into();

    Ok((Box::pin(server), listen_addresses))
}

#[cfg(test)]
pub mod tests {
    use async_trait::async_trait;
    use martin_tile_utils::{Encoding, Format, TileInfo};
    use tilejson::TileJSON;

    use super::*;
    use crate::source::{Source, TileData};
    use crate::{TileCoord, UrlQuery};

    #[derive(Debug, Clone)]
    pub struct TestSource {
        pub id: &'static str,
        pub tj: TileJSON,
        pub data: TileData,
    }

    #[async_trait]
    impl Source for TestSource {
        fn get_id(&self) -> &str {
            self.id
        }

        fn get_tilejson(&self) -> &TileJSON {
            &self.tj
        }

        fn get_tile_info(&self) -> TileInfo {
            TileInfo::new(Format::Mvt, Encoding::Uncompressed)
        }

        fn clone_source(&self) -> Box<dyn Source> {
            unimplemented!()
        }

        async fn get_tile(
            &self,
            _xyz: TileCoord,
            _url_query: Option<&UrlQuery>,
        ) -> MartinResult<TileData> {
            Ok(self.data.clone())
        }
    }
}
