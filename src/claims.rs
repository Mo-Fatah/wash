// Copyright 2015-2018 Capital One Services, LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{
    keys::extract_keypair,
    util::{CommandOutput, OutputKind},
};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use nkeys::{KeyPair, KeyPairType};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};
use term_table::{
    row::Row,
    table_cell::{Alignment, TableCell},
    Table,
};
use wascap::{
    caps::*,
    jwt::{
        Account, Actor, CapabilityProvider, Claims, Operator, Token, TokenValidation, WascapEntity,
    },
    wasm::{days_from_now_to_jwt_time, sign_buffer_with_claims},
};

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum ClaimsCliCommand {
    /// Examine the capabilities of a WebAssembly module
    #[clap(name = "inspect")]
    Inspect(InspectCommand),
    /// Sign a WebAssembly module, specifying capabilities and other claims
    /// including expiration, tags, and additional metadata
    #[clap(name = "sign")]
    Sign(SignCommand),
    /// Generate a signed JWT by supplying basic token information, a signing seed key, and metadata
    #[clap(name = "token", subcommand)]
    Token(TokenCommand),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct InspectCommand {
    /// Path to signed actor module or OCI URL of signed actor module
    pub(crate) module: String,

    /// Extract the raw JWT from the file and print to stdout
    #[clap(name = "jwt_only", long = "jwt-only")]
    jwt_only: bool,

    /// Digest to verify artifact against (if OCI URL is provided for <module>)
    #[clap(short = 'd', long = "digest")]
    digest: Option<String>,

    /// Allow latest artifact tags (if OCI URL is provided for <module>)
    #[clap(long = "allow-latest")]
    allow_latest: bool,

    /// OCI username, if omitted anonymous authentication will be used
    #[clap(
        short = 'u',
        long = "user",
        env = "WASH_REG_USER",
        hide_env_values = true
    )]
    user: Option<String>,

    /// OCI password, if omitted anonymous authentication will be used
    #[clap(
        short = 'p',
        long = "password",
        env = "WASH_REG_PASSWORD",
        hide_env_values = true
    )]
    password: Option<String>,

    /// Allow insecure (HTTP) registry connections
    #[clap(long = "insecure")]
    insecure: bool,

    /// skip the local OCI cache
    #[clap(long = "no-cache")]
    no_cache: bool,
}

#[derive(Parser, Debug, Clone)]
pub(crate) struct SignCommand {
    /// File to read
    pub(crate) source: String,

    /// Destination for signed module. If this flag is not provided, the signed module will be placed in the same directory as the source with a "_s" suffix
    #[clap(short = 'd', long = "destination")]
    destination: Option<String>,

    #[clap(flatten)]
    metadata: ActorMetadata,
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum TokenCommand {
    /// Generate a signed JWT for an actor module
    #[clap(name = "actor")]
    Actor(ActorMetadata),
    /// Generate a signed JWT for an operator
    #[clap(name = "operator")]
    Operator(OperatorMetadata),
    /// Generate a signed JWT for an account
    #[clap(name = "account")]
    Account(AccountMetadata),
    /// Generate a signed JWT for a service (capability provider)
    #[clap(name = "provider")]
    Provider(ProviderMetadata),
}

#[derive(Debug, Clone, Parser, Serialize, Deserialize)]
struct GenerateCommon {
    /// Location of key files for signing. Defaults to $WASH_KEYS ($HOME/.wash/keys)
    #[clap(long = "directory", env = "WASH_KEYS", hide_env_values = true)]
    directory: Option<PathBuf>,

    /// Indicates the token expires in the given amount of days. If this option is left off, the token will never expire
    #[clap(short = 'x', long = "expires")]
    expires_in_days: Option<u64>,

    /// Period in days that must elapse before this token is valid. If this option is left off, the token will be valid immediately
    #[clap(short = 'b', long = "nbf")]
    not_before_days: Option<u64>,

    /// Disables autogeneration of keys if seed(s) are not provided
    #[clap(long = "disable-keygen")]
    disable_keygen: bool,
}

#[derive(Debug, Clone, Parser)]
pub(crate) struct OperatorMetadata {
    /// A descriptive name for the operator
    #[clap(short = 'n', long = "name")]
    name: String,

    /// Path to issuer seed key (self signing operator). If this flag is not provided, the will be sourced from $WASH_KEYS ($HOME/.wash/keys) or generated for you if it cannot be found.
    #[clap(
        short = 'i',
        long = "issuer",
        env = "WASH_ISSUER_KEY",
        hide_env_values = true
    )]
    issuer: Option<String>,

    /// Additional keys to add to valid signers list
    /// Can either be seed value or path to seed file
    #[clap(short = 'a', long = "additional-key", name = "additional-keys")]
    additional_signing_keys: Option<Vec<String>>,

    #[clap(flatten)]
    common: GenerateCommon,
}

#[derive(Debug, Clone, Parser)]
pub(crate) struct AccountMetadata {
    /// A descriptive name for the account
    #[clap(short = 'n', long = "name")]
    name: String,

    /// Path to issuer seed key (operator). If this flag is not provided, the will be sourced from $WASH_KEYS ($HOME/.wash/keys) or generated for you if it cannot be found.
    #[clap(
        short = 'i',
        long = "issuer",
        env = "WASH_ISSUER_KEY",
        hide_env_values = true
    )]
    issuer: Option<String>,

    /// Path to subject seed key (account). If this flag is not provided, the will be sourced from $WASH_KEYS ($HOME/.wash/keys) or generated for you if it cannot be found.
    #[clap(
        short = 's',
        long = "subject",
        env = "WASH_SUBJECT_KEY",
        hide_env_values = true
    )]
    subject: Option<String>,

    /// Additional keys to add to valid signers list.
    /// Can either be seed value or path to seed file
    #[clap(short = 'a', long = "additional-key", name = "additional-keys")]
    additional_signing_keys: Option<Vec<String>>,

    #[clap(flatten)]
    common: GenerateCommon,
}

#[derive(Debug, Clone, Parser)]
pub(crate) struct ProviderMetadata {
    /// A descriptive name for the provider
    #[clap(short = 'n', long = "name")]
    name: String,

    /// Capability contract ID that this provider supports
    #[clap(short = 'c', long = "capid")]
    capid: String,

    /// A human-readable string identifying the vendor of this provider (e.g. Redis or Cassandra or NATS etc)
    #[clap(short = 'v', long = "vendor")]
    vendor: String,

    /// Monotonically increasing revision number
    #[clap(short = 'r', long = "revision")]
    revision: Option<i32>,

    /// Human-friendly version string
    #[clap(short = 'e', long = "version")]
    version: Option<String>,

    /// Path to issuer seed key (account). If this flag is not provided, the will be sourced from $WASH_KEYS ($HOME/.wash/keys) or generated for you if it cannot be found.
    #[clap(
        short = 'i',
        long = "issuer",
        env = "WASH_ISSUER_KEY",
        hide_env_values = true
    )]
    issuer: Option<String>,

    /// Path to subject seed key (service). If this flag is not provided, the will be sourced from $WASH_KEYS ($HOME/.wash/keys) or generated for you if it cannot be found.
    #[clap(
        short = 's',
        long = "subject",
        env = "WASH_SUBJECT_KEY",
        hide_env_values = true
    )]
    subject: Option<String>,

    #[clap(flatten)]
    common: GenerateCommon,
}

#[derive(Parser, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ActorMetadata {
    /// Enable the Key/Value Store standard capability
    #[clap(short = 'k', long = "keyvalue")]
    keyvalue: bool,
    /// Enable the Message broker standard capability
    #[clap(short = 'g', long = "msg")]
    msg_broker: bool,
    /// Enable the HTTP server standard capability
    #[clap(short = 'q', long = "http_server")]
    http_server: bool,
    /// Enable the HTTP client standard capability
    #[clap(long = "http_client")]
    http_client: bool,
    /// Enable access to the blob store capability
    #[clap(short = 'f', long = "blob_store")]
    blob_store: bool,
    /// Enable access to the extras functionality (random nos, guids, etc)
    #[clap(short = 'z', long = "extras")]
    extras: bool,
    /// Enable access to logging capability
    #[clap(short = 'l', long = "logging")]
    logging: bool,
    /// Enable access to an append-only event stream provider
    #[clap(short = 'e', long = "events")]
    eventstream: bool,
    /// A human-readable, descriptive name for the token
    #[clap(short = 'n', long = "name")]
    name: String,
    /// Add custom capabilities
    #[clap(short = 'c', long = "cap", name = "capabilities")]
    custom_caps: Vec<String>,
    /// A list of arbitrary tags to be embedded in the token
    #[clap(short = 't', long = "tag")]
    tags: Vec<String>,
    /// Indicates whether the signed module is a capability provider instead of an actor (the default is actor)
    #[clap(short = 'p', long = "prov")]
    provider: bool,
    /// Revision number
    #[clap(short = 'r', long = "rev")]
    rev: Option<i32>,
    /// Human-readable version string
    #[clap(short = 'v', long = "ver")]
    ver: Option<String>,
    /// Developer or human friendly unique alias used for invoking an actor, consisting of lowercase alphanumeric characters, underscores '_' and slashes '/'
    #[clap(short = 'a', long = "call-alias")]
    call_alias: Option<String>,

    /// Path to issuer seed key (account). If this flag is not provided, the will be sourced from $WASH_KEYS ($HOME/.wash/keys) or generated for you if it cannot be found.
    #[clap(
        short = 'i',
        long = "issuer",
        env = "WASH_ISSUER_KEY",
        hide_env_values = true
    )]
    issuer: Option<String>,

    /// Path to subject seed key (module). If this flag is not provided, the will be sourced from $WASH_KEYS ($HOME/.wash/keys) or generated for you if it cannot be found.
    #[clap(
        short = 's',
        long = "subject",
        env = "WASH_SUBJECT_KEY",
        hide_env_values = true
    )]
    subject: Option<String>,

    #[clap(flatten)]
    common: GenerateCommon,
}

pub(crate) async fn handle_command(
    command: ClaimsCliCommand,
    output_kind: OutputKind,
) -> Result<CommandOutput> {
    match command {
        ClaimsCliCommand::Inspect(inspectcmd) => render_caps(inspectcmd).await,
        ClaimsCliCommand::Sign(signcmd) => sign_file(signcmd, output_kind),
        ClaimsCliCommand::Token(gencmd) => generate_token(gencmd, output_kind),
    }
}

fn generate_token(cmd: TokenCommand, output_kind: OutputKind) -> Result<CommandOutput> {
    match cmd {
        TokenCommand::Actor(actor) => generate_actor(actor, output_kind),
        TokenCommand::Operator(operator) => generate_operator(operator, output_kind),
        TokenCommand::Account(account) => generate_account(account, output_kind),
        TokenCommand::Provider(provider) => generate_provider(provider, output_kind),
    }
}

fn get_keypair_vec(
    keys: &[String],
    keys_dir: Option<PathBuf>,
    keypair_type: KeyPairType,
    disable_keygen: bool,
    output_kind: OutputKind,
) -> Vec<KeyPair> {
    keys.iter()
        .map(|k| {
            extract_keypair(
                Some(k.to_string()),
                None,
                keys_dir.clone(),
                keypair_type.clone(),
                disable_keygen,
                output_kind,
            )
            .unwrap()
        })
        .collect()
}

fn generate_actor(actor: ActorMetadata, output_kind: OutputKind) -> Result<CommandOutput> {
    let issuer = extract_keypair(
        actor.issuer.clone(),
        Some(actor.name.clone()),
        actor.common.directory.clone(),
        KeyPairType::Account,
        actor.common.disable_keygen,
        output_kind,
    )?;
    let subject = extract_keypair(
        actor.subject.clone(),
        Some(actor.name.clone()),
        actor.common.directory.clone(),
        KeyPairType::Module,
        actor.common.disable_keygen,
        output_kind,
    )?;

    let mut caps_list = vec![];
    if actor.keyvalue {
        caps_list.push(wascap::caps::KEY_VALUE.to_string());
    }
    if actor.msg_broker {
        caps_list.push(wascap::caps::MESSAGING.to_string());
    }
    if actor.http_client {
        caps_list.push(wascap::caps::HTTP_CLIENT.to_string());
    }
    if actor.http_server {
        caps_list.push(wascap::caps::HTTP_SERVER.to_string());
    }
    if actor.blob_store {
        caps_list.push(wascap::caps::BLOB.to_string());
    }
    if actor.logging {
        caps_list.push(wascap::caps::LOGGING.to_string());
    }
    if actor.eventstream {
        caps_list.push(wascap::caps::EVENTSTREAMS.to_string());
    }
    caps_list.extend(actor.custom_caps.iter().cloned());

    if actor.provider && caps_list.len() > 1 {
        bail!("Capability providers cannot provide multiple capabilities at once.");
    }
    let claims: Claims<Actor> = Claims::<Actor>::with_dates(
        actor.name.clone(),
        issuer.public_key(),
        subject.public_key(),
        Some(caps_list),
        Some(actor.tags.clone()),
        days_from_now_to_jwt_time(actor.common.expires_in_days),
        days_from_now_to_jwt_time(actor.common.not_before_days),
        actor.provider,
        actor.rev,
        actor.ver.clone(),
        sanitize_alias(actor.call_alias)?,
    );

    let jwt = claims.encode(&issuer)?;

    Ok(CommandOutput::from_key_and_text("token", jwt))
}

fn generate_operator(operator: OperatorMetadata, output_kind: OutputKind) -> Result<CommandOutput> {
    let self_sign_key = extract_keypair(
        operator.issuer.clone(),
        Some(operator.name.clone()),
        operator.common.directory.clone(),
        KeyPairType::Operator,
        operator.common.disable_keygen,
        output_kind,
    )?;

    let additional_keys = match operator.additional_signing_keys.clone() {
        Some(keys) => get_keypair_vec(
            &keys,
            operator.common.directory.clone(),
            KeyPairType::Operator,
            true,
            output_kind,
        ),
        None => vec![],
    };

    let claims: Claims<Operator> = Claims::<Operator>::with_dates(
        operator.name.clone(),
        self_sign_key.public_key(),
        self_sign_key.public_key(),
        days_from_now_to_jwt_time(operator.common.not_before_days),
        days_from_now_to_jwt_time(operator.common.expires_in_days),
        if !additional_keys.is_empty() {
            additional_keys.iter().map(|k| k.public_key()).collect()
        } else {
            vec![]
        },
    );

    let jwt = claims.encode(&self_sign_key)?;

    Ok(CommandOutput::from_key_and_text("token", jwt))
}

fn generate_account(account: AccountMetadata, output_kind: OutputKind) -> Result<CommandOutput> {
    let issuer = extract_keypair(
        account.issuer.clone(),
        Some(account.name.clone()),
        account.common.directory.clone(),
        KeyPairType::Operator,
        account.common.disable_keygen,
        output_kind,
    )?;
    let subject = extract_keypair(
        account.subject.clone(),
        Some(account.name.clone()),
        account.common.directory.clone(),
        KeyPairType::Account,
        account.common.disable_keygen,
        output_kind,
    )?;
    let additional_keys = match account.additional_signing_keys.clone() {
        Some(keys) => get_keypair_vec(
            &keys,
            account.common.directory.clone(),
            KeyPairType::Account,
            true,
            output_kind,
        ),
        None => vec![],
    };

    let claims: Claims<Account> = Claims::<Account>::with_dates(
        account.name.clone(),
        issuer.public_key(),
        subject.public_key(),
        days_from_now_to_jwt_time(account.common.not_before_days),
        days_from_now_to_jwt_time(account.common.expires_in_days),
        if !additional_keys.is_empty() {
            additional_keys.iter().map(|k| k.public_key()).collect()
        } else {
            vec![]
        },
    );
    let jwt = claims.encode(&issuer)?;
    Ok(CommandOutput::from_key_and_text("token", jwt))
}

fn generate_provider(provider: ProviderMetadata, output_kind: OutputKind) -> Result<CommandOutput> {
    let issuer = extract_keypair(
        provider.issuer.clone(),
        Some(provider.name.clone()),
        provider.common.directory.clone(),
        KeyPairType::Account,
        provider.common.disable_keygen,
        output_kind,
    )?;
    let subject = extract_keypair(
        provider.subject.clone(),
        Some(provider.name.clone()),
        provider.common.directory.clone(),
        KeyPairType::Service,
        provider.common.disable_keygen,
        output_kind,
    )?;

    let claims: Claims<CapabilityProvider> = Claims::<CapabilityProvider>::with_dates(
        provider.name.clone(),
        issuer.public_key(),
        subject.public_key(),
        provider.capid.clone(),
        provider.vendor.clone(),
        provider.revision,
        provider.version.clone(),
        HashMap::new(),
        days_from_now_to_jwt_time(provider.common.not_before_days),
        days_from_now_to_jwt_time(provider.common.expires_in_days),
    );
    let jwt = claims.encode(&issuer)?;
    Ok(CommandOutput::from_key_and_text("token", jwt))
}

fn sign_file(cmd: SignCommand, output_kind: OutputKind) -> Result<CommandOutput> {
    let mut sfile = File::open(&cmd.source)
        .with_context(|| format!("Failed to open file for signing '{}'", &cmd.source))?;
    let mut buf = Vec::new();
    sfile.read_to_end(&mut buf).unwrap();

    let issuer = extract_keypair(
        cmd.metadata.issuer.clone(),
        Some(cmd.source.clone()),
        cmd.metadata.common.directory.clone(),
        KeyPairType::Account,
        cmd.metadata.common.disable_keygen,
        output_kind,
    )?;
    let subject = extract_keypair(
        cmd.metadata.subject.clone(),
        Some(cmd.source.clone()),
        cmd.metadata.common.directory.clone(),
        KeyPairType::Module,
        cmd.metadata.common.disable_keygen,
        output_kind,
    )?;

    let mut caps_list = vec![];
    if cmd.metadata.keyvalue {
        caps_list.push(wascap::caps::KEY_VALUE.to_string());
    }
    if cmd.metadata.msg_broker {
        caps_list.push(wascap::caps::MESSAGING.to_string());
    }
    if cmd.metadata.http_client {
        caps_list.push(wascap::caps::HTTP_CLIENT.to_string());
    }
    if cmd.metadata.http_server {
        caps_list.push(wascap::caps::HTTP_SERVER.to_string());
    }
    if cmd.metadata.blob_store {
        caps_list.push(wascap::caps::BLOB.to_string());
    }
    if cmd.metadata.logging {
        caps_list.push(wascap::caps::LOGGING.to_string());
    }
    if cmd.metadata.eventstream {
        caps_list.push(wascap::caps::EVENTSTREAMS.to_string());
    }
    caps_list.extend(cmd.metadata.custom_caps.iter().cloned());

    if cmd.metadata.provider && caps_list.len() > 1 {
        bail!("Capability providers cannot provide multiple capabilities at once.");
    }

    let signed = sign_buffer_with_claims(
        cmd.metadata.name.clone(),
        &buf,
        subject,
        issuer,
        cmd.metadata.common.expires_in_days,
        cmd.metadata.common.not_before_days,
        caps_list.clone(),
        cmd.metadata.tags.clone(),
        cmd.metadata.provider,
        cmd.metadata.rev,
        cmd.metadata.ver.clone(),
        sanitize_alias(cmd.metadata.call_alias)?,
    )?;

    let destination = match cmd.destination.clone() {
        Some(d) => d,
        None => {
            let path = PathBuf::from(cmd.source.clone())
                .parent()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let module_name = PathBuf::from(cmd.source.clone())
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            // If path is empty, user supplied module in current directory
            if path.is_empty() {
                format!("./{}_s.wasm", module_name)
            } else {
                format!("{}/{}_s.wasm", path, module_name)
            }
        }
    };

    let mut outfile = File::create(&destination).unwrap();

    let output = match outfile.write(&signed) {
        Ok(_) => {
            let mut map = HashMap::new();
            map.insert("destination".to_string(), json!(destination));
            map.insert("capabilities".to_string(), json!(caps_list));
            Ok(CommandOutput::new(
                format!(
                    "Successfully signed {} with capabilities: {}",
                    destination,
                    caps_list.join(",")
                ),
                map,
            ))
        }

        Err(e) => Err(e),
    }?;

    Ok(output)
}

async fn get_caps(cmd: &InspectCommand) -> Result<Option<Token<Actor>>> {
    let artifact_bytes = crate::reg::get_artifact(
        cmd.module.to_string(),
        cmd.digest.clone(),
        cmd.allow_latest,
        cmd.user.clone(),
        cmd.password.clone(),
        cmd.insecure,
        cmd.no_cache,
    )
    .await?;

    // Extract will return an error if it encounters an invalid hash in the claims
    Ok(wascap::wasm::extract_claims(&artifact_bytes)?)
}

async fn render_caps(cmd: InspectCommand) -> Result<CommandOutput> {
    let caps = get_caps(&cmd).await?;

    let out = match caps {
        Some(token) => {
            if cmd.jwt_only {
                CommandOutput::from_key_and_text("token", token.jwt)
            } else {
                let validation = wascap::jwt::validate_token::<Actor>(&token.jwt)?;
                render_actor_claims(token.claims, validation)
            }
        }
        None => bail!("No capabilities discovered in : {}", &cmd.module),
    };
    Ok(out)
}

/// Renders actor claims into provided output format
pub(crate) fn render_actor_claims(
    claims: Claims<Actor>,
    validation: TokenValidation,
) -> CommandOutput {
    let md = claims.metadata.clone().unwrap();
    let friendly_rev = md.rev.unwrap_or(0);
    let friendly_ver = md.ver.unwrap_or_else(|| "None".to_string());
    let friendly = format!("{} ({})", friendly_ver, friendly_rev);
    let provider = if md.provider {
        "Capability Provider"
    } else {
        "Capabilities"
    };

    let tags = if let Some(tags) = &claims.metadata.as_ref().unwrap().tags {
        if tags.is_empty() {
            "None".to_string()
        } else {
            tags.join(",")
        }
    } else {
        "None".to_string()
    };

    let friendly_caps: Vec<String> = if let Some(caps) = &claims.metadata.as_ref().unwrap().caps {
        caps.iter().map(|c| capability_name(c)).collect()
    } else {
        vec![]
    };

    let call_alias = claims
        .metadata
        .as_ref()
        .unwrap()
        .call_alias
        .clone()
        .unwrap_or_else(|| "(Not set)".to_string());

    let iss_label = token_label(&claims.issuer).to_ascii_lowercase();
    let sub_label = token_label(&claims.subject).to_ascii_lowercase();
    let provider_json = provider.replace(' ', "_").to_ascii_lowercase();

    let mut map = HashMap::new();
    map.insert(iss_label, json!(claims.issuer));
    map.insert(sub_label, json!(claims.subject));
    map.insert("expires".to_string(), json!(validation.expires_human));
    map.insert(
        "can_be_used".to_string(),
        json!(validation.not_before_human),
    );
    map.insert("version".to_string(), json!(friendly_ver));
    map.insert("revision".to_string(), json!(friendly_rev));
    map.insert(provider_json, json!(friendly_caps));
    map.insert("tags".to_string(), json!(tags));
    map.insert("call_alias".to_string(), json!(call_alias));

    let mut table = render_core(&claims, validation);

    table.add_row(Row::new(vec![
        TableCell::new("Version"),
        TableCell::new_with_alignment(friendly, 1, Alignment::Right),
    ]));

    table.add_row(Row::new(vec![
        TableCell::new("Call Alias"),
        TableCell::new_with_alignment(call_alias, 1, Alignment::Right),
    ]));

    table.add_row(Row::new(vec![TableCell::new_with_alignment(
        provider,
        2,
        Alignment::Center,
    )]));

    table.add_row(Row::new(vec![TableCell::new_with_alignment(
        friendly_caps.join("\n"),
        2,
        Alignment::Left,
    )]));

    table.add_row(Row::new(vec![TableCell::new_with_alignment(
        "Tags",
        2,
        Alignment::Center,
    )]));

    table.add_row(Row::new(vec![TableCell::new_with_alignment(
        tags,
        2,
        Alignment::Left,
    )]));

    CommandOutput::new(table.render(), map)
}

// * - we don't need render impls for Operator or Account because those tokens are never embedded into a module,
// only actors.

fn token_label(pk: &str) -> String {
    match pk.chars().next().unwrap() {
        'A' => "Account".to_string(),
        'M' => "Module".to_string(),
        'O' => "Operator".to_string(),
        'S' => "Server".to_string(),
        'U' => "User".to_string(),
        _ => "<Unknown>".to_string(),
    }
}

fn render_core<T>(claims: &Claims<T>, validation: TokenValidation) -> Table
where
    T: serde::Serialize + DeserializeOwned + WascapEntity,
{
    let mut table = Table::new();
    crate::util::configure_table_style(&mut table);

    let headline = format!("{} - {}", claims.name(), token_label(&claims.subject));
    table.add_row(Row::new(vec![TableCell::new_with_alignment(
        headline,
        2,
        Alignment::Center,
    )]));

    table.add_row(Row::new(vec![
        TableCell::new(token_label(&claims.issuer)),
        TableCell::new_with_alignment(&claims.issuer, 1, Alignment::Right),
    ]));
    table.add_row(Row::new(vec![
        TableCell::new(token_label(&claims.subject)),
        TableCell::new_with_alignment(&claims.subject, 1, Alignment::Right),
    ]));

    table.add_row(Row::new(vec![
        TableCell::new("Expires"),
        TableCell::new_with_alignment(validation.expires_human, 1, Alignment::Right),
    ]));

    table.add_row(Row::new(vec![
        TableCell::new("Can Be Used"),
        TableCell::new_with_alignment(validation.not_before_human, 1, Alignment::Right),
    ]));

    table
}

fn sanitize_alias(call_alias: Option<String>) -> Result<Option<String>> {
    if let Some(alias) = call_alias {
        // Alias cannot be a public key to ensure best practices
        if alias.is_empty() {
            bail!("Call alias cannot be empty")
        } else if alias.len() == 56
            && alias
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        {
            bail!("Public key cannot be used as a call alias")
        // Valid aliases contain a combination of lowercase alphanumeric characters, dashes, and slashes
        } else if alias
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/')
        {
            Ok(Some(alias))
        } else {
            bail!("Call alias contained invalid characters.\nValid aliases are lowercase alphanumeric and can contain underscores and slashes")
        }
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cmd {
        #[clap(subcommand)]
        claims: ClaimsCliCommand,
    }

    const SUBSCRIBER_OCI: &str = "wasmcloud.azurecr.io/subscriber:0.2.0";

    #[test]
    fn test_claims_sanitize_alias() {
        const VALID_ALPHANUMERIC: &str = "abc123";
        const VALID_WITHSYMBOLS: &str = "myorganization/subfolder_three";
        const INVALID_SYMBOLS: &str = "hello*^&%@#";
        const INVALID_CAPITAL: &str = "wasmCloud/camelCase_";
        const INVALID_PKEY: &str = "MCUOUQQP3WK4EWO76DPWIEKXMN4JYZ63KEGIEEHZCNBR2GEIXPB4ZFUT";

        assert_eq!(
            sanitize_alias(Some(VALID_ALPHANUMERIC.to_string()))
                .unwrap()
                .unwrap(),
            VALID_ALPHANUMERIC
        );
        assert_eq!(
            sanitize_alias(Some(VALID_WITHSYMBOLS.to_string()))
                .unwrap()
                .unwrap(),
            VALID_WITHSYMBOLS
        );

        let invalid_message = "Call alias contained invalid characters.\nValid aliases are lowercase alphanumeric and can contain underscores and slashes";
        let invalid_symbols = sanitize_alias(Some(INVALID_SYMBOLS.to_string()));
        match invalid_symbols {
            Err(e) => assert_eq!(format!("{}", e), invalid_message),
            _ => panic!("invalid symbols in call alias should not be accepted"),
        };

        let invalid_uppercase = sanitize_alias(Some(INVALID_CAPITAL.to_string()));
        match invalid_uppercase {
            Err(e) => assert_eq!(format!("{}", e), invalid_message),
            _ => panic!("uppercase symbols in call alias should not be accepted"),
        };

        let pkey_message = "Public key cannot be used as a call alias";
        let invalid_pkey = sanitize_alias(Some(INVALID_PKEY.to_string()));
        match invalid_pkey {
            Err(e) => assert_eq!(format!("{}", e), pkey_message),
            _ => panic!("public keys cannot be a call alias"),
        };

        let empty_message = "Call alias cannot be empty";
        let invalid_empty = sanitize_alias(Some("".to_string()));
        match invalid_empty {
            Err(e) => assert_eq!(format!("{}", e), empty_message),
            _ => panic!("call alias cannot be left empty"),
        }

        assert!(sanitize_alias(None).unwrap().is_none());
    }

    #[test]
    /// Enumerates all options and flags of the `claims inspect` command
    /// to ensure command line arguments do not change between versions
    fn test_claims_inspect_comprehensive() {
        let cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "inspect",
            SUBSCRIBER_OCI,
            "--digest",
            "sha256:5790f650cff526fcbc1271107a05111a6647002098b74a9a5e2e26e3c0a116b8",
            "--user",
            "name",
            "--password",
            "opensesame",
            "--allow-latest",
            "--insecure",
            "--jwt-only",
            "--no-cache",
        ])
        .unwrap();

        match cmd.claims {
            ClaimsCliCommand::Inspect(InspectCommand {
                module,
                jwt_only,
                digest,
                allow_latest,
                user,
                password,
                insecure,
                no_cache,
            }) => {
                assert_eq!(module, SUBSCRIBER_OCI);
                assert_eq!(
                    digest.unwrap(),
                    "sha256:5790f650cff526fcbc1271107a05111a6647002098b74a9a5e2e26e3c0a116b8"
                );
                assert_eq!(user.unwrap(), "name");
                assert_eq!(password.unwrap(), "opensesame");
                assert!(allow_latest);
                assert!(insecure);
                assert!(jwt_only);
                assert!(no_cache);
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }

        let short_cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "inspect",
            SUBSCRIBER_OCI,
            "-d",
            "sha256:5790f650cff526fcbc1271107a05111a6647002098b74a9a5e2e26e3c0a116b8",
            "-u",
            "name",
            "-p",
            "opensesame",
            "--allow-latest",
            "--insecure",
            "--jwt-only",
            "--no-cache",
        ])
        .unwrap();

        match short_cmd.claims {
            ClaimsCliCommand::Inspect(InspectCommand {
                module,
                jwt_only,
                digest,
                allow_latest,
                user,
                password,
                insecure,
                no_cache,
            }) => {
                assert_eq!(module, SUBSCRIBER_OCI);
                assert_eq!(
                    digest.unwrap(),
                    "sha256:5790f650cff526fcbc1271107a05111a6647002098b74a9a5e2e26e3c0a116b8"
                );
                assert_eq!(user.unwrap(), "name");
                assert_eq!(password.unwrap(), "opensesame");
                assert!(allow_latest);
                assert!(insecure);
                assert!(jwt_only);
                assert!(no_cache);
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }
    }

    #[test]
    /// Enumerates all options and flags of the `claims sign` command
    /// to ensure command line arguments do not change between versions
    fn test_claims_sign_comprehensive() {
        const LOCAL_WASM: &str = "./myactor.wasm";
        const ISSUER_KEY: &str = "SAAOBYD6BLELXSNN4S3TXUM7STGPB3A5HYU3D5T7XA4WHGVQBDBD4LJPOM";
        const SUBJECT_KEY: &str = "SMAMA4ABHIJUYQR54BDFHEMXIIGQATUXK6RYU6XLTFHDNCRVWT4KSDDSVE";
        let long_cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "sign",
            LOCAL_WASM,
            "--name",
            "MyActor",
            "--cap",
            "test:custom",
            "--destination",
            "./myactor_s.wasm",
            "--directory",
            "./dir",
            "--expires",
            "3",
            "--issuer",
            ISSUER_KEY,
            "--subject",
            SUBJECT_KEY,
            "--nbf",
            "1",
            "--rev",
            "2",
            "--tag",
            "testtag",
            "--ver",
            "0.0.1",
            "--blob_store",
            "--events",
            "--extras",
            "--http_client",
            "--http_server",
            "--keyvalue",
            "--logging",
            "--msg",
            "--prov",
            "--disable-keygen",
        ])
        .unwrap();

        match long_cmd.claims {
            ClaimsCliCommand::Sign(SignCommand {
                source,
                destination,
                metadata,
            }) => {
                assert_eq!(source, LOCAL_WASM);
                assert_eq!(destination.unwrap(), "./myactor_s.wasm");
                assert_eq!(metadata.common.directory.unwrap(), PathBuf::from("./dir"));
                assert_eq!(metadata.common.expires_in_days.unwrap(), 3);
                assert_eq!(metadata.common.not_before_days.unwrap(), 1);
                assert!(metadata.common.disable_keygen);
                assert!(metadata.keyvalue);
                assert!(metadata.msg_broker);
                assert!(metadata.http_server);
                assert!(metadata.http_client);
                assert!(metadata.blob_store);
                assert!(metadata.extras);
                assert!(metadata.logging);
                assert!(metadata.eventstream);
                assert_eq!(metadata.name, "MyActor");
                assert!(!metadata.custom_caps.is_empty());
                assert_eq!(metadata.custom_caps[0], "test:custom");
                assert!(!metadata.tags.is_empty());
                assert_eq!(metadata.tags[0], "testtag");
                assert!(metadata.provider);
                assert_eq!(metadata.rev.unwrap(), 2);
                assert_eq!(metadata.ver.unwrap(), "0.0.1");
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }
        let short_cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "sign",
            LOCAL_WASM,
            "-n",
            "MyActor",
            "-c",
            "test:custom",
            "-d",
            "./myactor_s.wasm",
            "--directory",
            "./dir",
            "-x",
            "3",
            "-i",
            ISSUER_KEY,
            "-s",
            SUBJECT_KEY,
            "-b",
            "1",
            "-r",
            "2",
            "-t",
            "testtag",
            "-v",
            "0.0.1",
            "-f",
            "-e",
            "-z",
            "-q",
            "-k",
            "-l",
            "-g",
            "-p",
            "--disable-keygen",
        ])
        .unwrap();

        match short_cmd.claims {
            ClaimsCliCommand::Sign(SignCommand {
                source,
                destination,
                metadata,
            }) => {
                assert_eq!(source, LOCAL_WASM);
                assert_eq!(destination.unwrap(), "./myactor_s.wasm");
                assert_eq!(metadata.common.directory.unwrap(), PathBuf::from("./dir"));
                assert_eq!(metadata.common.expires_in_days.unwrap(), 3);
                assert_eq!(metadata.common.not_before_days.unwrap(), 1);
                assert!(metadata.common.disable_keygen);
                assert!(metadata.keyvalue);
                assert!(metadata.msg_broker);
                assert!(metadata.http_server);
                assert!(metadata.blob_store);
                assert!(metadata.extras);
                assert!(metadata.logging);
                assert!(metadata.eventstream);
                assert_eq!(metadata.name, "MyActor");
                assert!(!metadata.custom_caps.is_empty());
                assert_eq!(metadata.custom_caps[0], "test:custom");
                assert!(!metadata.tags.is_empty());
                assert_eq!(metadata.tags[0], "testtag");
                assert!(metadata.provider);
                assert_eq!(metadata.rev.unwrap(), 2);
                assert_eq!(metadata.ver.unwrap(), "0.0.1");
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }
    }

    #[test]
    /// Enumerates all options and flags of the `claims sign` command
    /// to ensure command line arguments do not change between versions
    fn test_claims_token_comprehensive() {
        const DIR: &str = "./tests/fixtures";
        const EXPR: &str = "10";
        const NBFR: &str = "12";
        const OPERATOR_KEY: &str = "SOALSFXSHRVKCNOP2JSOVOU267XMF2ZMLF627OM6ZPS6WMKVS6HKQGU7QM";
        const OPERATOR_TWO_KEY: &str = "SOAC7EGQIMNPUF3XBSWR2IQIX7ITDNRYZZ4PN3ZZTFEVHPMG7BFOJMGPW4";
        const ACCOUNT_KEY: &str = "SAAH3WW3NDAT7GQOO5IHPHNIGS5JNFQN2F72P6QBSHCOKPBLEEDXQUWI4Q";
        const ACTOR_KEY: &str = "SMAA2XB7UP7FZLPLO27NJB65PKYISNQAH7PZ6PJUHR6CUARVANXZ4OTZOU";
        const PROVIDER_KEY: &str = "SVAKIVYER6D2LZS7QJFOU7LQYLRAMJ5DZE4B7BJHX6QFJIY24KN43JZGN4";

        let account_cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "token",
            "account",
            "--name",
            "TokenName",
            "--directory",
            DIR,
            "--expires",
            EXPR,
            "--nbf",
            NBFR,
            "--disable-keygen",
            "--issuer",
            OPERATOR_KEY,
            "--subject",
            ACCOUNT_KEY,
            "-a",
            OPERATOR_TWO_KEY,
        ])
        .unwrap();
        match account_cmd.claims {
            ClaimsCliCommand::Token(TokenCommand::Account(AccountMetadata {
                name,
                issuer,
                subject,
                common,
                additional_signing_keys,
                ..
            })) => {
                assert_eq!(name, "TokenName");
                assert_eq!(common.directory.unwrap(), PathBuf::from(DIR));
                assert_eq!(
                    common.expires_in_days.unwrap(),
                    EXPR.parse::<u64>().unwrap()
                );
                assert_eq!(
                    common.not_before_days.unwrap(),
                    NBFR.parse::<u64>().unwrap()
                );
                assert!(common.disable_keygen);
                assert_eq!(issuer.unwrap(), OPERATOR_KEY);
                assert_eq!(subject.unwrap(), ACCOUNT_KEY);
                let adds = additional_signing_keys.unwrap();
                assert_eq!(adds.len(), 1);
                assert_eq!(adds[0], OPERATOR_TWO_KEY);
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }
        let actor_cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "token",
            "actor",
            "--name",
            "TokenName",
            "--directory",
            DIR,
            "--expires",
            EXPR,
            "--nbf",
            NBFR,
            "--disable-keygen",
            "--issuer",
            ACCOUNT_KEY,
            "--subject",
            ACTOR_KEY,
            "-c",
            "test:custom",
            "--rev",
            "2",
            "--tag",
            "testtag",
            "--ver",
            "0.0.1",
            "--blob_store",
            "--events",
            "--extras",
            "--http_client",
            "--http_server",
            "--keyvalue",
            "--logging",
            "--msg",
        ])
        .unwrap();
        match actor_cmd.claims {
            ClaimsCliCommand::Token(TokenCommand::Actor(ActorMetadata {
                name,
                issuer,
                subject,
                common,
                keyvalue,
                msg_broker,
                http_server,
                http_client,
                blob_store,
                extras,
                logging,
                eventstream,
                custom_caps,
                tags,
                rev,
                ver,
                ..
            })) => {
                assert_eq!(name, "TokenName");
                assert_eq!(common.directory.unwrap(), PathBuf::from(DIR));
                assert_eq!(
                    common.expires_in_days.unwrap(),
                    EXPR.parse::<u64>().unwrap()
                );
                assert_eq!(
                    common.not_before_days.unwrap(),
                    NBFR.parse::<u64>().unwrap()
                );
                assert!(common.disable_keygen);
                assert_eq!(issuer.unwrap(), ACCOUNT_KEY);
                assert_eq!(subject.unwrap(), ACTOR_KEY);
                assert!(keyvalue);
                assert!(msg_broker);
                assert!(http_server);
                assert!(http_client);
                assert!(blob_store);
                assert!(extras);
                assert!(logging);
                assert!(eventstream);
                assert_eq!(custom_caps.len(), 1);
                assert_eq!(custom_caps[0], "test:custom");
                assert!(!tags.is_empty());
                assert_eq!(tags[0], "testtag");
                assert_eq!(rev.unwrap(), 2);
                assert_eq!(ver.unwrap(), "0.0.1");
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }
        let operator_cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "token",
            "operator",
            "--name",
            "TokenName",
            "--directory",
            DIR,
            "--expires",
            EXPR,
            "--nbf",
            NBFR,
            "--disable-keygen",
            "--issuer",
            OPERATOR_KEY,
            "--additional-key",
            OPERATOR_TWO_KEY,
        ])
        .unwrap();
        match operator_cmd.claims {
            ClaimsCliCommand::Token(TokenCommand::Operator(OperatorMetadata {
                name,
                issuer,
                common,
                additional_signing_keys,
                ..
            })) => {
                assert_eq!(name, "TokenName");
                assert_eq!(common.directory.unwrap(), PathBuf::from(DIR));
                assert_eq!(
                    common.expires_in_days.unwrap(),
                    EXPR.parse::<u64>().unwrap()
                );
                assert_eq!(
                    common.not_before_days.unwrap(),
                    NBFR.parse::<u64>().unwrap()
                );
                assert!(common.disable_keygen);
                assert_eq!(issuer.unwrap(), OPERATOR_KEY);
                let adds = additional_signing_keys.unwrap();
                assert_eq!(adds.len(), 1);
                assert_eq!(adds[0], OPERATOR_TWO_KEY);
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }
        let provider_cmd: Cmd = Parser::try_parse_from(&[
            "claims",
            "token",
            "provider",
            "--name",
            "TokenName",
            "--directory",
            DIR,
            "--expires",
            EXPR,
            "--nbf",
            NBFR,
            "--disable-keygen",
            "--issuer",
            ACCOUNT_KEY,
            "--subject",
            PROVIDER_KEY,
            "--capid",
            "wasmcloud:test",
            "--vendor",
            "test",
            "--revision",
            "0",
            "--version",
            "1.2.3",
        ])
        .unwrap();
        match provider_cmd.claims {
            ClaimsCliCommand::Token(TokenCommand::Provider(ProviderMetadata {
                name,
                issuer,
                subject,
                common,
                capid,
                vendor,
                revision,
                version,
                ..
            })) => {
                assert_eq!(name, "TokenName");
                assert_eq!(common.directory.unwrap(), PathBuf::from(DIR));
                assert_eq!(
                    common.expires_in_days.unwrap(),
                    EXPR.parse::<u64>().unwrap()
                );
                assert_eq!(
                    common.not_before_days.unwrap(),
                    NBFR.parse::<u64>().unwrap()
                );
                assert!(common.disable_keygen);
                assert_eq!(issuer.unwrap(), ACCOUNT_KEY);
                assert_eq!(subject.unwrap(), PROVIDER_KEY);
                assert_eq!(capid, "wasmcloud:test");
                assert_eq!(vendor, "test");
                assert_eq!(revision.unwrap(), 0);
                assert_eq!(version.unwrap(), "1.2.3");
            }
            cmd => panic!("claims constructed incorrect command: {:?}", cmd),
        }
    }
}
