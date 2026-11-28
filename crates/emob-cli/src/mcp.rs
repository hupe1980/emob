//! The offline receipt verifier as a read-only tool on the host's stdio pipes.

use emob_verify::Verdict;
use rmcp::{
    Json, ServerHandler, ServiceExt as _,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
    transport::{async_rw::AsyncRwTransport, stdio},
};

const RECORD_LIMIT: usize = 65_536;
const KEY_LIMIT: usize = 1_024;
const FRAME_LIMIT: usize = 512 * 1024;

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MeterRecord {
    /// Verbatim OCMF record, including its signature.
    #[schemars(length(min = 1, max = RECORD_LIMIT))]
    record: String,
    /// Public meter key from an independent source, such as the charge point.
    #[schemars(length(min = 1, max = KEY_LIMIT))]
    public_key: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SignatureStatus {
    Verified,
    Mismatch,
    Inconclusive,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct SignatureCheck {
    status: SignatureStatus,
    explanation: &'static str,
}

struct Verifier {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Verifier {
    #[tool(
        description = "Verify one OCMF meter record offline against the public key obtained from the charge point.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn verify_meter_record(
        &self,
        Parameters(input): Parameters<MeterRecord>,
    ) -> Result<Json<SignatureCheck>, String> {
        if !(1..=RECORD_LIMIT).contains(&input.record.chars().count())
            || !(1..=KEY_LIMIT).contains(&input.public_key.chars().count())
        {
            return Err(
                "Supply one record (1–65536 characters) and one public key (1–1024 characters)."
                    .into(),
            );
        }
        let verdict = emob_verify::check(&input.record, &input.public_key).verdict;
        let status = match verdict {
            Verdict::Verified => SignatureStatus::Verified,
            Verdict::NotVerified | Verdict::KeyMismatch => SignatureStatus::Mismatch,
            Verdict::Unavailable | Verdict::UnreadableRecord | Verdict::UnreadableKey => {
                SignatureStatus::Inconclusive
            }
        };
        Ok(Json(SignatureCheck {
            status,
            explanation: verdict.sentence(),
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Verifier {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("emob", env!("CARGO_PKG_VERSION")))
            .with_instructions("Supply the record and the public key printed on the meter. A verified signature establishes which key signed the bytes.")
    }
}

#[tokio::main(flavor = "current_thread")]
pub(super) async fn serve() -> anyhow::Result<()> {
    let (input, output) = stdio();
    let transport = AsyncRwTransport::new_server(input, output).with_max_line_length(FRAME_LIMIT);
    let service = Verifier {
        tool_router: Verifier::tool_router(),
    }
    .serve(transport)
    .await?;
    if let rmcp::service::QuitReason::JoinError(error) = service.waiting().await? {
        return Err(error.into());
    }
    Ok(())
}
