use dotenv::dotenv;
use mokshamint::lightning::{LightningType, LndLightningSettings};
use mokshamint::{MintBuilder, run_server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    let lnd_settings = envy::prefixed("LND_")
        .from_env::<LndLightningSettings>()
        .expect("Please provide lnd info");

    let ln_type = LightningType::Lnd(lnd_settings);

    let mint = MintBuilder::new()
        .with_db("postgres://postgres:postgres@localhost:5432/moksha-mint".to_string())
        .with_fee(0f32, 4000)
        .with_lightning(ln_type)
        .with_private_key("my_private_key".to_string())
        .build()
        .await;

    let host_port = "[::]:3338".to_string().parse().expect("Invalid host port");
    run_server(mint?, host_port, None, None).await
}