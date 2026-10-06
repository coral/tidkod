//! Generated convenience layers over the ABI bridges, from the same facade AST.
use super::{args, output, suffix};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
pub(super) fn snake(name: &str) -> String {
    name.chars()
        .enumerate()
        .flat_map(|(i, c)| {
            let mut s = String::new();
            if c.is_uppercase() && i > 0 {
                s.push('_');
            }
            s.push(c.to_ascii_lowercase());
            s.chars().collect::<Vec<_>>()
        })
        .collect()
}
fn camel(name: &str) -> String {
    let mut parts = name.split('_');
    let mut out = parts.next().unwrap().to_string();
    for p in parts {
        let mut c = p.chars();
        if let Some(first) = c.next() {
            out.extend(first.to_uppercase());
            out.extend(c);
        }
    }
    out
}
pub(super) fn base(t: &str) -> &str {
    t.trim_start_matches('&').trim_start_matches("mut")
}
pub(super) fn scalar(t: &str, swift: bool) -> String {
    match (t, swift) {
        ("()", true) => "Void",
        ("()", false) => "void",
        ("bool", true) => "Bool",
        ("bool", false) => "bool",
        ("u8", true) => "UInt8",
        ("u32", true) => "UInt32",
        ("u64", true) => "UInt64",
        ("i32", true) => "Int32",
        ("i64", true) => "Int64",
        ("f32", true) => "Float",
        ("f64", true) => "Double",
        ("u8", false) => "uint8_t",
        ("u32", false) => "uint32_t",
        ("u64", false) => "uint64_t",
        ("i32", false) => "int32_t",
        ("i64", false) => "int64_t",
        ("f32", false) => "float",
        ("f64", false) => "double",
        _ => t,
    }
    .into()
}
pub fn generate(
    dir: &Path,
    opaque: &BTreeSet<String>,
    records: &BTreeMap<String, Vec<(String, String)>>,
    functions: &[syn::ItemFn],
) {
    let owner = |name: &str| {
        opaque
            .iter()
            .filter(|t| name.starts_with(&(snake(t) + "_")))
            .max_by_key(|t| t.len())
            .expect("function needs an owning handle")
            .clone()
    };
    let enums = [
        (
            "Synchronization",
            "synchronization",
            vec!["uninitialized", "acquiring", "synchronized", "holdover"],
        ),
        (
            "ConnectionState",
            "connection",
            vec!["disconnected", "connecting", "connected", "shutdown"],
        ),
        ("SourceKind", "source_kind", vec!["generated", "tracked"]),
        ("SourceHealth", "source_health", vec!["healthy", "degraded"]),
    ];
    if std::env::var_os("CARGO_FEATURE_SWIFT").is_some() {
        let mut s = String::from(
            "// Generated; do not edit.\nimport TidkodSys\nimport Foundation\npublic struct TidkodError: Error, CustomStringConvertible { public let description: String }\nprivate func checked<T>(_ body: () throws -> T) throws -> T { do { return try body() } catch let error as RustString { throw TidkodError(description: error.toString()) } catch { throw error } }\n",
        );
        for (name, _, variants) in &enums {
            s += &format!("public enum {name}: UInt8 {{\n");
            for (i, variant) in variants.iter().enumerate() {
                s += &format!("case `{variant}` = {i}\n");
            }
            s += "case unknown = 255\n}\n";
        }
        for (name, fields) in records {
            s += &format!("public struct {name} {{ fileprivate let raw: TidkodSys.{name}\n");
            for (n, t) in fields {
                if let Some((kind, _, _)) = enums
                    .iter()
                    .find(|(_, field, _)| name == "Reading" && field == n)
                {
                    s += &format!(
                        "public var `{}`: {kind} {{ {kind}(rawValue: raw.{n}) ?? .unknown }}\n",
                        camel(n)
                    );
                } else {
                    s += &format!(
                        "public var `{}`: {} {{ raw.{n} }}\n",
                        camel(n),
                        scalar(t, true)
                    );
                }
            }
            if name == "Reading" {
                s += "public var timecode: String { String(format: \"%02d:%02d:%02d%@%02d\", hours, minutes, seconds, dropFrame ? \";\" : \":\", frame) }\n";
            }
            s += "}\n";
        }
        for t in opaque {
            // The native Swift SDK owns system Bonjour. The low-level Rust/C ABI
            // remains generated unchanged; Discovery's Swift surface is adapted.
            if t == "Discovery" {
                continue;
            }
            let declaration = if t == "Endpoint" {
                "struct"
            } else {
                "final class"
            };
            s += &format!(
                "/// Owned handle. Serialize access; this type is deliberately not Sendable.\npublic {declaration} {t} {{ fileprivate let raw: TidkodSys.{t}\nfileprivate init(raw: TidkodSys.{t}) {{ self.raw = raw }}\n"
            );
            if t == "Engine" {
                s += "fileprivate let bonjour = BonjourContext()\n";
            }
            if t == "Leader" {
                s += "fileprivate var bonjourAdvertisement: BonjourAdvertisement?\npublic var advertisementStatus: BonjourStatus { bonjourAdvertisement?.status ?? .disabled }\ndeinit { bonjourAdvertisement?.stop() }\n";
            }
            for f in functions
                .iter()
                .filter(|f| owner(&f.sig.ident.to_string()) == *t)
            {
                let name = f.sig.ident.to_string();
                if name == "engine_leader_external_discovery" {
                    // Adapter SPI remains available in TidkodSys, not the normal SDK.
                    continue;
                }
                let method = name.strip_prefix(&(snake(t) + "_")).unwrap();
                let a = args(f);
                let instance = a
                    .first()
                    .is_some_and(|(_, ty)| ty.starts_with('&') && base(ty) == t);
                let a_rest = &a[usize::from(instance)..];
                let (ret, fallible) = output(f);
                let init = method == "new";
                let typ = |ty: &str| match ty {
                    "String" | "&str" => "String".into(),
                    "Vec<u8>" | "&[u8]" => "[UInt8]".into(),
                    "&[f32]" => "UnsafeBufferPointer<Float>".into(),
                    "&mut[f32]" => "UnsafeMutableBufferPointer<Float>".into(),
                    _ => scalar(base(ty), true),
                };
                let decl = a_rest
                    .iter()
                    .map(|(n, ty)| format!("`{}`: {}", camel(n), typ(ty)))
                    .collect::<Vec<_>>()
                    .join(", ");
                s += &if init {
                    format!(
                        "public convenience init({decl}) {} {{\n",
                        if fallible { "throws" } else { "" }
                    )
                } else {
                    format!(
                        "public {}func `{}`({decl}) {} -> {} {{\n",
                        if instance { "" } else { "static " },
                        camel(method),
                        if fallible { "throws" } else { "" },
                        typ(&ret)
                    )
                };
                let adapter = match name.as_str() {
                    "engine_leader" => Some(
                        "try bonjour.checkRunning()\nlet leader = Leader(raw: try checked { try TidkodSys.engine_leader_external_discovery(raw, options.raw) })\ndo { leader.bonjourAdvertisement = try bonjour.advertise(leader.advertisement()); return leader } catch { try? leader.shutdown(); throw error }\n",
                    ),
                    "engine_discovery" => Some(
                        "return try Discovery(context: bonjour, engine: raw, interfaces: [])\n",
                    ),
                    "engine_discovery_configured" => Some(
                        "let interfaces = try (0..<options.interfaceCount()).map { try options.interfaceName(index: $0) }\nreturn try Discovery(context: bonjour, engine: raw, interfaces: interfaces)\n",
                    ),
                    "follower_options_discovered" => Some(
                        "return try discovery.resolvedOptions(index: index, addressIndex: addressIndex)\n",
                    ),
                    _ => None,
                };
                if let Some(body) = adapter {
                    s += body;
                    s += "}\n";
                    continue;
                }
                if name == "engine_shutdown" {
                    s += "bonjour.shutdown()\n";
                }
                if name == "leader_shutdown" {
                    s += "bonjourAdvertisement?.stop()\n";
                }
                let callargs = a
                    .iter()
                    .enumerate()
                    .map(|(i, (n, ty))| {
                        if instance && i == 0 {
                            "raw".into()
                        } else if opaque.contains(base(ty)) {
                            format!("`{}`.raw", camel(n))
                        } else if ty == "&[f32]" || ty == "&mut[f32]" {
                            format!("`{0}`.baseAddress ?? {1}<Float>(bitPattern: MemoryLayout<Float>.alignment)!, UInt(`{0}`.count)", camel(n), if ty=="&[f32]" {"UnsafePointer"}else{"UnsafeMutablePointer"})
                        } else if ty == "&[u8]" {
                            "buffer".into()
                        } else {
                            format!("`{}`", camel(n))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut expr = format!(
                    "{}TidkodSys.{name}({callargs})",
                    if fallible { "try " } else { "" }
                );
                if let Some((n, _)) = a_rest.iter().find(|(_, ty)| ty == "&[u8]") {
                    expr = format!(
                        "{}`{}`.withUnsafeBufferPointer {{ buffer in {expr} }}",
                        if fallible { "try " } else { "" },
                        camel(n)
                    );
                }
                if fallible {
                    expr = format!("try checked {{ {expr} }}");
                }
                if init {
                    s += &format!("self.init(raw: {expr})\n");
                } else if ret == "()" {
                    s += &format!("{expr}\n");
                } else {
                    s += &format!(
                        "let value = {expr}\nreturn {}\n",
                        if opaque.contains(&ret) || records.contains_key(&ret) {
                            format!("{ret}(raw: value)")
                        } else if ret == "String" {
                            "value.toString()".into()
                        } else if ret == "Vec<u8>" {
                            "Array(value)".into()
                        } else {
                            "value".into()
                        }
                    );
                }
                s += "}\n";
            }
            s += "}\n";
        }
        if opaque.contains("Endpoint") {
            s += include_str!("templates/Endpoint.swift");
        }
        if opaque.contains("Engine") {
            s += include_str!("templates/Bonjour.swift");
            s += include_str!("templates/PresentationReader.swift");
        }
        fs::write(dir.join("TidkodClient.swift"), s).unwrap();
    }
    if std::env::var_os("CARGO_FEATURE_CPP").is_some() {
        super::cpp::generate(dir, opaque, records, functions);
    }
    if std::env::var_os("CARGO_FEATURE_C").is_some() {
        let mut s = String::from(
            "/* Generated; owned structs must not be copied. */\n#ifndef TIDKOD_CLIENT_H\n#define TIDKOD_CLIENT_H\n#include \"tidkod.h\"\n#include <string.h>\n",
        );
        for t in opaque {
            s += &format!(
                "typedef struct {{ {t} *raw; }} TK{t};\nstatic inline void tk_{}_dispose(TK{t} *v) {{ tidkod_{}_free(v->raw); v->raw = NULL; }}\n",
                snake(t),
                t.to_lowercase()
            );
        }
        s += "typedef struct { const uint8_t *data; size_t len; } TKBytes;\nstatic inline void tk_buffer_dispose(TKBuffer **value) { tidkod_buffer_free(*value); *value = NULL; }\nstatic inline TKBytes tk_buffer_view(const TKBuffer *value) { TKBytes bytes = {tidkod_buffer_data(value), tidkod_buffer_len(value)}; return bytes; }\n";
        s += "typedef struct {const float *data; size_t len;} TKFloatInput;\ntypedef struct {float *data; size_t len;} TKFloatOutput;\n";
        let ty = |t: &str| {
            if opaque.contains(t) {
                format!("TK{t}")
            } else if t == "String" || t == "Vec<u8>" {
                "TKBuffer *".into()
            } else {
                scalar(t, false)
            }
        };
        let returns: BTreeSet<_> = functions.iter().map(|f| output(f).0).collect();
        for t in returns {
            s += &format!(
                "typedef struct {{ int32_t status; TKBuffer *error; {} }} TKResult{};\n",
                if t == "()" {
                    String::new()
                } else {
                    format!("{} value;", ty(&t))
                },
                suffix(&t)
            );
        }
        for f in functions {
            let name = f.sig.ident.to_string();
            let a = args(f);
            let (ret, _) = output(f);
            let result = format!("TKResult{}", suffix(&ret));
            let decl = a
                .iter()
                .map(|(n, t)| {
                    format!(
                        "{} {n}",
                        if t == "&str" {
                            "const char *".into()
                        } else if t == "&[f32]" {
                            "TKFloatInput".into()
                        } else if t == "&mut[f32]" {
                            "TKFloatOutput".into()
                        } else if t == "&[u8]" {
                            "TKBytes".into()
                        } else {
                            ty(base(t))
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut callargs = a
                .iter()
                .map(|(n, t)| {
                    if t == "&str" {
                        format!("(const uint8_t*){n}, {n} ? strlen({n}) : 0")
                    } else if matches!(t.as_str(), "&[u8]" | "&[f32]" | "&mut[f32]") {
                        format!("{n}.data, {n}.len")
                    } else if opaque.contains(base(t)) {
                        format!("{n}.raw")
                    } else {
                        n.clone()
                    }
                })
                .collect::<Vec<_>>();
            if ret != "()" {
                callargs.push(if opaque.contains(&ret) {
                    "&result.value.raw".into()
                } else {
                    "&result.value".into()
                });
            }
            callargs.push("&result.error".into());
            s += &format!(
                "static inline {result} tk_{name}({}) {{ {result} result = {{0}}; result.status = tidkod_{name}({}); return result; }}\n",
                if decl.is_empty() { "void" } else { &decl },
                callargs.join(", ")
            );
        }
        s += "#endif\n";
        fs::write(dir.join("tidkod-client.h"), s).unwrap();
    }
}
