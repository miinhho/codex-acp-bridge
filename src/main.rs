mod agent;
mod approval;
mod codex;
mod history;
mod projection;
mod prompt;

use anyhow::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let app_server = codex::AppServer::spawn().await?;
    agent::serve(app_server).await?;
    Ok(())
}
