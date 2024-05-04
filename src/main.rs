use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue},
    response::IntoResponse,
    routing::get,
    Router,
};
use clap::Parser;
use futures::future::join_all;
use prometheus_client::{
    encoding::{text::encode, EncodeLabelSet},
    metrics::{counter::Counter, family::Family, gauge::Gauge},
    registry::Registry,
};
use reqwest::{multipart::Form, Error};
use serde::de::DeserializeOwned;
use serde_derive::{Deserialize, Serialize};
use tokio::{net::TcpListener, spawn, sync::Mutex};

const DEFAULT_PROMETHEUS_BIND_ADDR: &str = "[::1]:12345";

const PROMETHEUS_CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Address on which to expose metrics and web interface.
    #[arg(long = "web.listen-address", default_value = DEFAULT_PROMETHEUS_BIND_ADDR)]
    listen_address: String,

    /// Address of the Enphase Envoy on your local network.
    #[arg(long = "envoy.address")]
    envoy_address: String,

    /// Serial number of the Enphase Envoy (look up in the app).
    #[arg(long = "envoy.serial")]
    envoy_serial: String,

    /// Enphase Envoy username (look up in the app).
    #[arg(long = "envoy.username", env = "ENVOY_USERNAME")]
    envoy_username: String,

    /// Enphase Envoy username.
    #[arg(long = "envoy.password", env = "ENVOY_PASSWORD")]
    envoy_password: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();

    let client = Client::new(
        &args.envoy_address,
        &args.envoy_username,
        &args.envoy_password,
        &args.envoy_serial,
    );

    eprintln!("listening on {}", &args.listen_address);

    let app = Router::new()
        .route("/metrics", get(metrics))
        .with_state(AppState::new(client));

    let listener = TcpListener::bind(&args.listen_address)
        .await
        .expect("error binding to the listen address");

    axum::serve(listener, app)
        .await
        .expect("error running server");
}

#[derive(Clone)]
struct AppState {
    client: Client,
    registry: Arc<Registry>,
    production_watts: Gauge<f64, AtomicU64>,
    inverter_production_watts: Family<InverterLabels, Gauge<f64, AtomicU64>>,
    lifetime_watt_hours: Counter<f64, AtomicU64>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct InverterLabels {
    serial_num: String,
}

impl AppState {
    fn new(client: Client) -> Self {
        let mut registry = Registry::default();

        let production_watts = Gauge::<f64, AtomicU64>::default();

        registry.register(
            "enphase_envoy_production_watts",
            "Currently produced watts",
            production_watts.clone(),
        );

        let inverter_production_watts = Family::<InverterLabels, Gauge<f64, AtomicU64>>::default();

        registry.register(
            "enphase_envoy_inverter_production_watts",
            "Last known production for inverters",
            inverter_production_watts.clone(),
        );

        let lifetime_watt_hours = Counter::<f64, AtomicU64>::default();

        registry.register(
            "enphase_envoy_lifetime_watt_hours",
            "Total amount of watt hours produced by the system",
            lifetime_watt_hours.clone(),
        );

        let registry = Arc::new(registry);

        Self {
            client,
            registry,
            production_watts,
            inverter_production_watts,
            lifetime_watt_hours,
        }
    }
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let mut updates = vec![];

    updates.push(spawn({
        let client = state.client.clone();
        async move {
            state.production_watts.set(
                client
                    .production_watts()
                    .await
                    .expect("error getting production value"),
            );
        }
    }));

    updates.push(spawn({
        let client = state.client.clone();
        async move {
            let inverter_production = client
                .inverter_production_watts()
                .await
                .expect("error getting inverter production");

            for inverter in inverter_production {
                let serial_num = inverter.serial_num;
                state
                    .inverter_production_watts
                    .get_or_create(&InverterLabels { serial_num })
                    .set(inverter.last_known_watts);
            }
        }
    }));

    updates.push(spawn({
        let client = state.client.clone();
        async move {
            state.lifetime_watt_hours.inner().store(
                client
                    .lifetime_watt_hours()
                    .await
                    .expect("error getting lifetime production value")
                    .to_bits(),
                Ordering::Relaxed,
            );
        }
    }));

    join_all(updates).await;

    let mut buffer = String::new();
    encode(&mut buffer, &state.registry).expect("error encoding prometheus data");

    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static(PROMETHEUS_CONTENT_TYPE),
    );

    (headers, buffer)
}

/// Ideally we'd use [enphase](https://docs.rs/enphase/) crate, but it relies
/// on valid TLS certificates, while Enphase self-signs theirs for Envoy.
#[derive(Clone)]
struct Client {
    hostname: String,
    username: String,
    password: String,
    serial_num: String,
    client: reqwest::Client,
    token: Arc<Mutex<Option<String>>>,
}

impl Client {
    fn new(
        hostname: impl AsRef<str>,
        username: impl AsRef<str>,
        password: impl AsRef<str>,
        serial_num: impl AsRef<str>,
    ) -> Self {
        let hostname = hostname.as_ref().into();
        let username = username.as_ref().into();
        let password = password.as_ref().into();
        let serial_num = serial_num.as_ref().into();

        let client = reqwest::ClientBuilder::new()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("error building reqwest client");

        let token = Arc::new(Mutex::new(None));

        Self {
            hostname,
            username,
            password,
            serial_num,
            client,
            token,
        }
    }

    async fn authenticate(&self) -> Result<String, Error> {
        let form = Form::new()
            .text("user[email]", self.username.clone())
            .text("user[password]", self.password.clone());

        let response = self
            .client
            .post("https://enlighten.enphaseenergy.com/login/login.json")
            .multipart(form)
            .send()
            .await?
            .error_for_status()?;

        let session_id = response.json::<LoginResponse>().await?.session_id;
        let username = self.username.clone();
        let serial_num = self.serial_num.clone();

        let response = self
            .client
            .post("https://entrez.enphaseenergy.com/tokens")
            .json(&TokenRequest {
                session_id,
                username,
                serial_num,
            })
            .send()
            .await?
            .error_for_status()?;

        response
            .bytes()
            .await
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
    }

    async fn token(&self) -> Result<String, Error> {
        let mut guard = self.token.lock().await;
        match &*guard {
            Some(token) => Ok(token.clone()),
            None => {
                let token = self.authenticate().await?;
                guard.replace(token.clone());
                Ok(token)
            }
        }
    }

    async fn production_watts(&self) -> Result<f64, Error> {
        self.get::<ProductionResponse>("/ivp/meters/reports/production")
            .await
            .map(|response| response.cumulative.current_watts)
    }

    async fn inverter_production_watts(&self) -> Result<Vec<InverterProduction>, Error> {
        self.get::<Vec<InverterProduction>>("/api/v1/production/inverters")
            .await
    }

    async fn lifetime_watt_hours(&self) -> Result<f64, Error> {
        self.get::<CumulativeProductionResponse>("/production.json")
            .await
            .map(|response| {
                response
                    .production
                    .into_iter()
                    .find(|item| item.kind == "inverters")
                    .map(|item| item.lifetime_watt_hours)
                    .unwrap_or_default()
            })
    }

    async fn get<R>(&self, path: &str) -> Result<R, Error>
    where
        R: DeserializeOwned,
    {
        let token = self.token().await?;

        let response = self
            .client
            .get(format!("https://{}{}", self.hostname, path,))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json::<R>()
            .await?;

        Ok(response)
    }
}

#[derive(Deserialize, Debug)]
struct LoginResponse {
    session_id: String,
}

#[derive(Serialize, Debug)]
struct TokenRequest {
    session_id: String,
    username: String,
    serial_num: String,
}

#[derive(Deserialize, Debug)]
struct ProductionResponse {
    cumulative: CumulativeProduction,
}

#[derive(Deserialize, Debug)]
struct CumulativeProduction {
    #[serde(rename = "currW")]
    current_watts: f64,
}

#[derive(Deserialize, Debug)]
struct InverterProduction {
    #[serde(rename = "serialNumber")]
    serial_num: String,
    #[serde(rename = "lastReportWatts")]
    last_known_watts: f64,
}

#[derive(Deserialize, Debug)]
struct CumulativeProductionResponse {
    production: Vec<CumulativeProductionResponseItem>,
}

#[derive(Deserialize, Debug)]
struct CumulativeProductionResponseItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "whLifetime")]
    lifetime_watt_hours: f64,
}
