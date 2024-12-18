use anyhow::{Context, Result};
use atrium_api::{
    app::bsky::feed::get_author_feed::{Parameters, ParametersData},
    types::{
        string::{AtIdentifier::Did, Datetime},
        TryFromUnknown,
    },
};
use bsky_sdk::api::app::bsky::feed::post::RecordData;
use bsky_sdk::BskyAgent;
use clap::{Parser, Subcommand};
use config::{Config, File};
use dialoguer::{theme::ColorfulTheme, Confirm};
use serde::Deserialize;

#[derive(Parser)]
#[command(version, about)]
struct Opts {
    /// Say 'yes' to all prompts.
    #[clap(short, long)]
    yes: bool,

    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Delete posts from a user following the configuration file.
    Delete {
        /// Configuration file.
        #[clap(value_parser)]
        config: String,
    },
}

#[derive(Deserialize, Debug)]
struct Authentication {
    /// BlueSky identifier.
    identifier: String,

    /// BlueSky app password from <https://bsky.app/settings/app-password>.
    app_password: String,
}

#[derive(Deserialize, Debug)]
struct Delete {
    /// Minimum age of a post to be considered for deletion.
    #[serde(deserialize_with = "duration_str::deserialize_duration_chrono")]
    minimum_age: chrono::Duration,
}

#[derive(Deserialize, Debug)]
struct Rules {
    /// When to delete posts.
    delete: Delete,
}

#[derive(Deserialize, Debug)]
struct Settings {
    /// Authentication settings.
    authentication: Authentication,

    /// Rules for deleting or keeping posts.
    rules: Rules,
}

impl Settings {
    pub fn from_file(path: &str) -> Result<Self> {
        Config::builder()
            .add_source(File::with_name(path))
            .build()
            .context(format!("Failed to build config from {path}"))?
            .try_deserialize()
            .context(format!("Failed to deserialize config from {path}"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    // Parse command line options and read the configuration file.
    let opts = Opts::parse();
    let config = match opts.command {
        Command::Delete { config } => config,
    };
    let settings = Settings::from_file(&config)?;

    // Log in to BlueSky.
    let agent = BskyAgent::builder().build().await?;
    agent
        .login(
            settings.authentication.identifier,
            settings.authentication.app_password,
        )
        .await?;

    // Get the DID of the logged in user (Decentralized Identifier).
    let did = agent
        .get_session()
        .await
        .expect("could not get DID for the logged in user")
        .did
        .clone();

    // Get all posts from the user
    let output = agent
        .api
        .app
        .bsky
        .feed
        .get_author_feed(Parameters {
            data: ParametersData {
                actor: Did(did.clone()),
                cursor: None,
                filter: None,
                include_pins: Some(false),
                limit: None,
            },
            extra_data: ipld_core::ipld::Ipld::Null,
        })
        .await?;

    // Collect the URIs of the records to delete.
    let mut records_to_delete = vec![];
    let cutoff_time =
        Datetime::new((chrono::Utc::now() - settings.rules.delete.minimum_age).into());
    for feed_view_post in &output.feed {
        // Map the ATProtocol generic data into the BlueSky specific RecordData type.
        let record = RecordData::try_from_unknown(feed_view_post.post.record.clone())?;
        if record.created_at > cutoff_time {
            // Skip posts that are too recent.
            continue;
        }

        if feed_view_post.post.author.did == did {
            records_to_delete.push(feed_view_post.post.uri.clone());
        } else {
            records_to_delete.push(
                feed_view_post
                    .post
                    .viewer
                    .as_ref()
                    .expect("empty viewer for repost")
                    .repost
                    .as_ref()
                    .expect("empty repost for viewer")
                    .clone(),
            );
        }
    }
    println!("About to delete {} records", records_to_delete.len());

    // Confirm deletion.
    if !opts.yes {
        let theme = ColorfulTheme::default();
        let prompt = Confirm::with_theme(&theme).with_prompt("Do you want to proceed?");
        if !prompt.interact()? {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Delete the records.
    for uri in records_to_delete {
        agent.delete_record(uri).await?;
    }

    Ok(())
}
