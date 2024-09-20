use opentelemetry::{
    runtime,
    sdk::{
        trace::{BatchConfig, RandomIdGenerator, Sampler, Tracer},
        Resource,
    },
    KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use opentelemetry_semantic_conventions::{
    resource::{SERVICE_NAME, SERVICE_VERSION},
    SCHEMA_URL,
};
use std::env::var;
use std::time::Duration;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

fn init_tracer(resource: Resource, endpoint: &str) -> Tracer {
    let export_config = ExportConfig {
        endpoint: endpoint.to_string(),
        timeout: Duration::from_secs(3),
        protocol: Protocol::Grpc,
    };

    opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_trace_config(
            opentelemetry::sdk::trace::Config::default()
                .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                    1.0,
                ))))
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(resource),
        )
        .with_batch_config(BatchConfig::default())
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(endpoint)
                .with_export_config(export_config),
        )
        .install_batch(runtime::Tokio)
        .unwrap()
}

fn init_tracing_subscriber(service_name: &str) -> OtelGuard {
    let resource = Resource::from_schema_url(
        [
            KeyValue::new(SERVICE_NAME, service_name.to_owned()),
            KeyValue::new(SERVICE_VERSION, "0.4.1"),
        ],
        SCHEMA_URL,
    );

    let env_filter = EnvFilter::from_default_env();

    let reg = tracing_subscriber::registry().with(env_filter).with(
        tracing_subscriber::fmt::layer()
            .with_thread_ids(true)
            .with_ansi(false)
            .compact(),
    );
    let _ = if let Ok(endpoint) = var("OTLP_ENDPOINT") {
        reg.with(OpenTelemetryLayer::new(init_tracer(resource, &endpoint)))
            .try_init()
    } else {
        reg.try_init()
    };

    log::debug!("Initialized tracing");

    OtelGuard {}
}

pub fn init_tracing(service_name: &str) -> OtelGuard {
    init_tracing_subscriber(service_name)
}

pub struct OtelGuard {}
impl Drop for OtelGuard {
    fn drop(&mut self) {
        opentelemetry::global::shutdown_tracer_provider();
    }
}
