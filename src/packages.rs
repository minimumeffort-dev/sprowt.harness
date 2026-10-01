use std::io;

use serde::Deserialize;
use serde_json::{Value, json};

pub const TOOL: &str = "install_system_packages";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub packages: Vec<String>,
    pub reason: String,
}

impl Request {
    pub fn parse(arguments: Value) -> io::Result<Self> {
        serde_json::from_value::<Self>(arguments)?.validate()
    }

    pub fn validate(mut self) -> io::Result<Self> {
        let request = &mut self;
        if request.packages.is_empty()
            || request.packages.len() > 64
            || request.reason.trim().is_empty()
            || request.reason.len() > 1000
            || request.packages.iter().any(|name| {
                name.len() < 2
                    || name.len() > 128
                    || !name.as_bytes()[0].is_ascii_lowercase()
                        && !name.as_bytes()[0].is_ascii_digit()
                    || !name.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || b"+.-".contains(&c)
                    })
            })
        {
            return Err(io::Error::other(
                "Request 1–64 Debian package names and a short reason. URLs, paths and options are not accepted.",
            ));
        }
        request.packages.sort();
        request.packages.dedup();
        Ok(self)
    }
}

pub fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Install required OS packages in this mod's Debian 12 Linux VM. Choose package names from project requirements or missing-library errors. The harness runs apt using signed official Debian repositories; normal worker commands stay read-only outside /workspace, /home/sprowt and /tmp. No custom repositories, URLs, shell commands, removals or blanket system upgrades. Return includes success or a setup error.",
        "inputSchema":{"type":"object","additionalProperties":false,
            "properties":{"packages":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":64},"reason":{"type":"string"}},
            "required":["packages","reason"]}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_names_but_rejects_commands_sources_and_options() {
        let request = Request::parse(
            json!({"packages":["libnss3","libglib2.0-0","libnss3"],"reason":"Chromium libraries"}),
        )
        .unwrap();
        assert_eq!(request.packages, ["libglib2.0-0", "libnss3"]);
        for name in [
            "",
            "-y",
            "/tmp/pkg.deb",
            "https://example.com/pkg",
            "libnss3;id",
            "libnss3=1",
            "libnss3:amd64",
            "lib nss3",
            "libnss3\n",
        ] {
            assert!(Request::parse(json!({"packages":[name],"reason":"test"})).is_err());
        }
        assert!(Request::parse(json!({"packages":[],"reason":"test"})).is_err());
        assert!(Request::parse(json!({"packages":["libnss3"],"reason":""})).is_err());
        assert!(
            Request::parse(json!({"packages":["libnss3"],"reason":"test","command":"id"})).is_err()
        );
    }
}
