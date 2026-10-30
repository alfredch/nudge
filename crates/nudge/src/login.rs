//! Interactive `login` and `logout`.

use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use nudge_api::{Client, DeviceId, DeviceName, Error, Login, Secret};

use crate::credentials::{self, Backend, Credentials};
use crate::paths::Paths;

pub const NOTICE: &str = "Nudge is an unofficial Pushover Open Client. \
                          It is not released or supported by Pushover.";

pub async fn login(paths: &Paths, user_agent: &str, new_device: bool) -> Result<()> {
    println!("{NOTICE}\n");
    let client = Client::new(user_agent)?;
    let existing = credentials::load(paths).await?.map(|(c, _)| c);

    let email = prompt("Email: ")?;
    let password = rpassword::prompt_password("Password: ").context("cannot read password")?;
    let Login { secret, .. } = match client.login(&email, &password, None).await {
        Err(Error::TwoFactorRequired) => {
            let code = prompt("Two-factor code: ")?;
            client.login(&email, &password, Some(&code)).await
        }
        other => other,
    }
    .map_err(explain)?;
    drop(password);

    let device_id = match existing {
        Some(c) if !new_device => {
            println!("Reusing device {}.", c.device_id);
            verify_device(&client, &secret, &c.device_id).await?;
            c.device_id
        }
        _ => register(&client, &secret).await?,
    };

    let backend = credentials::save(paths, &Credentials { device_id, secret }).await?;
    match backend {
        Backend::Keyring => println!("Logged in. Credentials are stored in the Secret Service."),
        Backend::File => println!(
            "Logged in. No Secret Service available; credentials are stored in {} (mode 0600).",
            paths.credentials_file().display()
        ),
    }
    println!(
        "\nNote: Open Clients count as desktop devices. A Pushover for Desktop license is \
         required within 30 days, see https://pushover.net/."
    );
    Ok(())
}

pub async fn logout(paths: &Paths) -> Result<()> {
    if credentials::delete(paths).await? {
        println!("Local credentials removed.");
    } else {
        println!("No stored credentials found.");
    }
    println!(
        "The device stays registered; remove it on https://pushover.net/ if you no longer need it."
    );
    Ok(())
}

async fn register(client: &Client, secret: &Secret) -> Result<DeviceId> {
    let default = DeviceName::sanitized(&gethostname::gethostname().to_string_lossy());
    loop {
        let input = prompt(&format!("Device name [{default}]: "))?;
        let name = if input.is_empty() {
            default.clone()
        } else {
            match DeviceName::new(input) {
                Ok(name) => name,
                Err(e) => {
                    println!("{e}");
                    continue;
                }
            }
        };
        match client.register_device(secret, &name).await {
            Ok(id) => {
                println!("Registered device {name}.");
                return Ok(id);
            }
            Err(Error::Api { errors, .. }) if !errors.is_empty() => {
                println!("Registration failed: {}", errors.join("; "));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// A fetch without delete shows whether the stored device still works.
async fn verify_device(client: &Client, secret: &Secret, device: &DeviceId) -> Result<()> {
    match client.fetch_messages(secret, device).await {
        Ok(_) => Ok(()),
        Err(e) if e.is_client_error() => {
            bail!("The stored device is no longer valid ({e}). Run `nudge login --new-device`.")
        }
        Err(e) => Err(e.into()),
    }
}

fn explain(err: Error) -> anyhow::Error {
    match err {
        Error::TwoFactorRequired => anyhow::anyhow!("two-factor code was not accepted"),
        other => other.into(),
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line)? == 0 {
        bail!("input closed");
    }
    Ok(line.trim().to_owned())
}
