//! Safe managed facade generated from the same API definitions as the C ABI.
use super::{args, output};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
fn pascal(s: &str) -> String {
    s.split('_')
        .map(|s| {
            let mut c = s.chars();
            c.next()
                .unwrap()
                .to_uppercase()
                .chain(c)
                .collect::<String>()
        })
        .collect()
}
fn snake(s: &str) -> String {
    s.chars()
        .enumerate()
        .map(|(i, c)| {
            if i > 0 && c.is_uppercase() {
                format!("_{}", c.to_lowercase())
            } else {
                c.to_lowercase().to_string()
            }
        })
        .collect()
}
fn base(s: &str) -> &str {
    s.trim_start_matches('&').trim_start_matches("mut")
}
fn ty(s: &str) -> String {
    match s {
        "()" => "void",
        "u8" => "byte",
        "u32" => "uint",
        "u64" => "ulong",
        "i32" => "int",
        "i64" => "long",
        "f32" => "float",
        "f64" => "double",
        "&str" | "String" => "string",
        "&[u8]" => "ReadOnlySpan<byte>",
        "&[f32]" => "ReadOnlySpan<float>",
        "&mut[f32]" => "Span<float>",
        "Vec<u8>" => "byte[]",
        _ => base(s),
    }
    .into()
}
pub fn generate(
    dir: &Path,
    opaque: &BTreeSet<String>,
    records: &BTreeMap<String, Vec<(String, String)>>,
    functions: &[syn::ItemFn],
) {
    let mut s = String::from(
        "// Generated from src/api.rs. Do not edit.\n#nullable enable\nusing System;\nusing System.Text;\nusing Microsoft.Win32.SafeHandles;\nusing Sys = Tidkod.Sys;\nnamespace Tidkod;\n",
    );
    for (name, fields) in records {
        s += &format!(
            "public readonly struct {name} {{ private readonly Sys.{name} raw; internal {name}(Sys.{name} value) {{ raw=value; }}\n"
        );
        for (n, t) in fields {
            let kind = match (name.as_str(), n.as_str()) {
                ("Reading", "connection") => Some("ConnectionState"),
                ("Reading", "synchronization") => Some("Synchronization"),
                ("Reading", "source_kind") => Some("SourceKind"),
                ("Reading", "source_health") => Some("SourceHealth"),
                _ => None,
            };
            s += &format!(
                "public {} {} => {}raw.@{n};\n",
                kind.map_or_else(|| ty(t), str::to_string),
                pascal(n),
                kind.map_or(String::new(), |k| format!("({k})"))
            );
        }
        if name == "Reading" {
            s += "public string Timecode => $\"{Hours:D2}:{Minutes:D2}:{Seconds:D2}{(DropFrame ? ';' : ':')}{Frame:D2}\";\n";
        }
        s += "}\n";
    }
    for name in opaque {
        s += &format!(
            "internal sealed unsafe class {name}Handle : OwnedHandle {{ internal {name}Handle(Sys.{name}* value):base() {{SetHandle((IntPtr)value);}} protected override bool ReleaseHandle() {{Sys.NativeMethods.tidkod_{}_free((Sys.{name}*)handle);return true;}} }}\n",
            name.to_lowercase()
        );
        s += &format!(
            "public sealed unsafe partial class {name} : IDisposable {{ internal readonly {name}Handle Handle; internal {name}(Sys.{name}* value) {{Handle=new {name}Handle(value);}} public void Dispose() => Handle.Dispose();\n"
        );
        for f in functions {
            let fname = f.sig.ident.to_string();
            let owner = opaque
                .iter()
                .filter(|t| fname.starts_with(&(snake(t) + "_")))
                .max_by_key(|t| t.len())
                .expect("API owner");
            if owner != name {
                continue;
            }
            let method = fname.strip_prefix(&(snake(name) + "_")).unwrap();
            let a: Vec<_> = args(f)
                .into_iter()
                .map(|(n, t)| {
                    let pascal = pascal(&n);
                    (format!("{}{}", pascal[..1].to_lowercase(), &pascal[1..]), t)
                })
                .collect();
            let instance = a
                .first()
                .is_some_and(|(_, t)| t.starts_with('&') && base(t) == name);
            let constructor =
                method == "new" || (name == "FollowerOptions" && method == "endpoint");
            let method = if method == "bind_endpoint" {
                "bind"
            } else {
                method
            };
            let (ret, _) = output(f);
            let decl = a[usize::from(instance)..]
                .iter()
                .map(|(n, t)| format!("{} @{n}", ty(t)))
                .collect::<Vec<_>>()
                .join(", ");
            s += &if constructor {
                format!("public {name}({decl}) {{\n")
            } else {
                format!(
                    "public {}{} {}({decl}) {{\n",
                    if instance { "" } else { "static " },
                    ty(&ret),
                    pascal(method)
                )
            };
            let mut call = Vec::new();
            let mut pinned = Vec::new();
            for (i, (n, t)) in a.iter().enumerate() {
                if opaque.contains(base(t)) {
                    let handle = if instance && i == 0 {
                        "Handle".into()
                    } else {
                        format!("@{n}.Handle")
                    };
                    if !(instance && i == 0) {
                        s += &format!("ArgumentNullException.ThrowIfNull(@{n});\n");
                    }
                    s += &format!("using var lease_{n} = new HandleLease({handle});\n");
                    call.push(format!("(Sys.{}*)lease_{n}.Pointer", base(t)));
                } else if t == "&[f32]" || t == "&mut[f32]" {
                    pinned.push(format!("fixed(float* ptr_{n}=@{n})"));
                    call.push(format!("ptr_{n}"));
                    call.push(format!("(nuint)@{n}.Length"));
                } else if t == "&str" || t == "&[u8]" {
                    let buffer = if t == "&str" {
                        s += &format!(
                            "ArgumentNullException.ThrowIfNull(@{n});\nvar bytes_{n}=Encoding.UTF8.GetBytes(@{n});\n"
                        );
                        format!("bytes_{n}")
                    } else {
                        format!("@{n}")
                    };
                    pinned.push(format!("fixed(byte* ptr_{n}={buffer})"));
                    call.push(format!("ptr_{n}"));
                    call.push(format!("(nuint){buffer}.Length"));
                } else {
                    call.push(format!("@{n}"));
                }
            }
            let rt = if opaque.contains(&ret) {
                format!("Sys.{ret}*")
            } else if records.contains_key(&ret) {
                format!("Sys.{ret}")
            } else if ret == "String" || ret == "Vec<u8>" {
                "Sys.TKBuffer*".into()
            } else {
                ty(&ret)
            };
            if ret != "()" {
                s += &format!("{rt} value=default;\n");
                call.push("&value".into());
            }
            s += "Sys.TKBuffer* error=null;\n";
            call.push("&error".into());
            for p in &pinned {
                s += &format!("{p} {{\n");
            }
            s += &format!(
                "Native.Check(Sys.NativeMethods.tidkod_{fname}({}),error);\n",
                call.join(",")
            );
            for _ in &pinned {
                s += "}\n";
            }
            if constructor {
                s += &format!("Handle=new {name}Handle(value);\n");
            } else if ret != "()" {
                s += &format!(
                    "return {};\n",
                    if opaque.contains(&ret) || records.contains_key(&ret) {
                        format!("new {ret}(value)")
                    } else if ret == "String" {
                        "Native.TakeString(value)".into()
                    } else if ret == "Vec<u8>" {
                        "Native.TakeBytes(value)".into()
                    } else {
                        "value".into()
                    }
                );
            }
            s += "}\n";
        }
        s += "}\n";
    }
    fs::write(dir.join("Tidkod.g.cs"), s).unwrap();
    fs::write(
        dir.join("Tidkod.Support.cs"),
        include_str!("templates/Tidkod.Support.cs"),
    )
    .unwrap();
    if opaque.contains("Endpoint") {
        fs::write(
            dir.join("Tidkod.Native.cs"),
            include_str!("templates/Tidkod.Native.cs"),
        )
        .unwrap();
    }
}
