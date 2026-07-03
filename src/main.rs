use std::env;

#[tokio::main]
async fn main() {
    helium_service::config::load_dotenv();

    if env::args().nth(1).as_deref() == Some("healthcheck") {
        if let Err(err) = healthcheck().await {
            eprintln!("{err}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(err) = helium_service::run().await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

async fn healthcheck() -> Result<(), String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| err.to_string())?;

    let response = client
        .get(helium_service::config::healthcheck_url())
        .send()
        .await
        .map_err(|err| err.to_string())?;

    if !(200..=399).contains(&response.status().as_u16()) {
        return Err(format!("unexpected status: {}", response.status()));
    }

    response.bytes().await.map_err(|err| err.to_string())?;
    Ok(())
}
