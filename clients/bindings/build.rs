//! Schema compiler for the public functions/records in src/api.rs. Deliberately
//! rejects unsupported types instead of silently generating a partial binding.
mod cpp;
mod csharp;
mod wrappers;
use quote::ToTokens;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
};
use syn::{FnArg, Item, Pat, ReturnType, Type, Visibility};
fn ty(t: &Type) -> String {
    t.to_token_stream().to_string().replace(' ', "")
}
fn enabled(attrs: &[syn::Attribute], native: bool) -> bool {
    for attr in attrs.iter().filter(|a| a.path().is_ident("cfg")) {
        assert_eq!(
            attr.meta.to_token_stream().to_string().replace(' ', ""),
            "cfg(feature=\"native\")",
            "unsupported binding feature gate"
        );
        if !native {
            return false;
        }
    }
    true
}
fn output(f: &syn::ItemFn) -> (String, bool) {
    let t = match &f.sig.output {
        ReturnType::Default => "()".into(),
        ReturnType::Type(_, t) => ty(t),
    };
    if t.starts_with("Result<") {
        (t[7..t.len() - 1].into(), true)
    } else {
        (t, false)
    }
}
fn args(f: &syn::ItemFn) -> Vec<(String, String)> {
    f.sig
        .inputs
        .iter()
        .map(|a| {
            let FnArg::Typed(a) = a else {
                panic!("receivers are not supported")
            };
            let Pat::Ident(p) = &*a.pat else {
                panic!("argument pattern unsupported")
            };
            (p.ident.to_string(), ty(&a.ty))
        })
        .collect()
}
fn suffix(t: &str) -> String {
    match t {
        "()" => "Unit".into(),
        "Vec<u8>" => "Bytes".into(),
        other => other.into(),
    }
}
fn main() {
    println!("cargo:rerun-if-changed=src/api.rs");
    println!("cargo:rerun-if-changed=src/c_support.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=wrappers.rs");
    println!("cargo:rerun-if-changed=csharp.rs");
    println!("cargo:rerun-if-changed=cpp.rs");
    println!("cargo:rerun-if-changed=templates");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-cdylib=-Wl,-install_name,@rpath/libtidkod_bindings.dylib");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-cdylib=-Wl,-soname,libtidkod_bindings.so");
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let native = env::var_os("CARGO_FEATURE_NATIVE").is_some();
    let parsed = syn::parse_file(&fs::read_to_string("src/api.rs").unwrap()).unwrap();
    let mut records = BTreeMap::new();
    let mut opaque = BTreeSet::new();
    let mut functions = Vec::new();
    for item in parsed.items {
        match item {
            Item::Struct(s)
                if matches!(s.vis, Visibility::Public(_)) && enabled(&s.attrs, native) =>
            {
                if s.fields
                    .iter()
                    .all(|f| matches!(f.vis, Visibility::Public(_)))
                {
                    records.insert(
                        s.ident.to_string(),
                        s.fields
                            .iter()
                            .map(|f| (f.ident.as_ref().unwrap().to_string(), ty(&f.ty)))
                            .collect::<Vec<_>>(),
                    );
                } else {
                    opaque.insert(s.ident.to_string());
                }
            }
            Item::Fn(f) if matches!(f.vis, Visibility::Public(_)) && enabled(&f.attrs, native) => {
                functions.push(f)
            }
            _ => {}
        }
    }
    let primitive = ["()", "bool", "u8", "u32", "u64", "i32", "i64", "f32", "f64"];
    let valid = |t: &str| {
        primitive.contains(&t)
            || records.contains_key(t)
            || opaque.contains(t)
            || matches!(t, "String" | "Vec<u8>")
    };
    for f in &functions {
        assert!(
            f.sig.asyncness.is_none()
                && f.sig.unsafety.is_none()
                && f.sig.generics.params.is_empty(),
            "foreign API functions must be safe, synchronous and non-generic"
        );
        let (t, _) = output(f);
        assert!(valid(&t), "unsupported return {t}");
        for (_, t) in args(f) {
            assert!(
                (primitive.contains(&t.as_str()) && t != "()")
                    || matches!(t.as_str(), "&str" | "&[u8]" | "&[f32]" | "&mut[f32]")
                    || (t.starts_with('&')
                        && opaque.contains(t.trim_start_matches('&').trim_start_matches("mut"))),
                "unsupported argument {t}"
            );
        }
    }
    for fields in records.values() {
        for (_, t) in fields {
            assert!(
                primitive.contains(&t.as_str()) && t != "()",
                "records must contain fixed-width primitives"
            );
        }
    }
    let fields = |name: &str| {
        records[name]
            .iter()
            .map(|(n, t)| format!("pub {n}: {t},"))
            .collect::<String>()
    };
    let convert = |name: &str| {
        format!(
            "impl From<crate::api::{name}> for ffi::{name} {{fn from(value:crate::api::{name})->Self{{Self{{{}}}}}}}",
            records[name]
                .iter()
                .map(|(n, _)| format!("{n}:value.{n},"))
                .collect::<String>()
        )
    };
    let mut manifest = String::from("Generated from src/api.rs. Do not edit generated files.\n");
    for f in &functions {
        manifest += &format!("{}\n", f.sig.to_token_stream());
    }
    fs::write(out.join("API.txt"), manifest).unwrap();
    if env::var_os("CARGO_FEATURE_SWIFT").is_some() {
        let mut bridge = String::from("#[swift_bridge::bridge] mod ffi {\n");
        let mut body = String::new();
        for name in records.keys() {
            bridge += &format!(
                "#[swift_bridge(swift_repr = \"struct\")] struct {name}{{{}}}\n",
                fields(name)
            );
            body += &convert(name);
        }
        bridge += "extern \"Rust\" {\n";
        for name in &opaque {
            bridge += &format!("type {name};\n");
            body += &format!("use crate::api::{name};\n");
        }
        for f in &functions {
            let name = f.sig.ident.to_string();
            let (t, result) = output(f);
            let a = args(f);
            let decl = a
                .iter()
                .map(|(n, t)| {
                    if t == "&[f32]" || t == "&mut[f32]" {
                        return format!(
                            "{n}:*{} f32,{n}_len:usize",
                            if t == "&[f32]" { "const" } else { "mut" }
                        );
                    }
                    format!(
                        "{n}:{}",
                        if t == "&str" {
                            "String".into()
                        } else {
                            t.replace("&mut", "&mut ")
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let call = format!(
                "crate::api::{name}({})",
                a.iter()
                    .map(|(n, t)| if t == "&str" {
                        format!("&{n}")
                    } else {
                        n.clone()
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let ret = if result {
                format!("Result<{t},String>")
            } else {
                t.clone()
            };
            bridge += &format!("fn {name}({decl})->{ret};\n");
            let ret = if records.contains_key(&t) {
                if result {
                    format!("Result<ffi::{t},String>")
                } else {
                    format!("ffi::{t}")
                }
            } else {
                ret
            };
            let expr = if records.contains_key(&t) {
                if result {
                    format!("{call}.map(Into::into)")
                } else {
                    format!("{call}.into()")
                }
            } else {
                call
            };
            let mut init = String::new();
            for (n, t) in &a {
                if t == "&[f32]" || t == "&mut[f32]" {
                    let (borrow, from) = if t == "&[f32]" {
                        ("&", "from_raw_parts")
                    } else {
                        ("&mut ", "from_raw_parts_mut")
                    };
                    init += &format!(
                        "let {n}=if {n}_len==0 {{{borrow}[]}} else {{unsafe{{std::slice::{from}({n},{n}_len)}}}};"
                    );
                }
            }
            body += &format!("fn {name}({decl})->{ret}{{{init}{expr}}}\n");
        }
        bridge += "}}\n";
        let file = out.join("swift.rs");
        fs::write(&file, (bridge + &body).replace("->()", "")).unwrap();
        swift_bridge_build::parse_bridges([&file])
            .write_all_concatenated(out.join("swift"), "Tidkod");
    }
    if env::var_os("CARGO_FEATURE_C").is_some() {
        let mut c = String::from("use crate::c_support::*;\n");
        for name in records.keys() {
            c += &format!(
                "#[repr(C)] #[derive(Default)] pub struct {name}{{{}}}\n",
                fields(name)
            );
        }
        for name in &opaque {
            c += &format!("pub struct {name}{{_private:()}}\n");
            c += &format!(
                "#[unsafe(no_mangle)] pub unsafe extern \"C\" fn tidkod_{}_free(value:*mut {name}){{if !value.is_null(){{unsafe{{drop(Box::from_raw(value.cast::<crate::api::{name}>()))}}}}}}\n",
                name.to_lowercase()
            );
        }
        for f in &functions {
            let name = f.sig.ident.to_string();
            let (t, result) = output(f);
            let mut decl = Vec::new();
            let mut init = String::new();
            let mut callargs = Vec::new();
            for (n, t) in args(f) {
                if t == "&str" || t == "&[u8]" {
                    decl.push(format!("{n}:*const u8,{n}_len:usize"));
                    init += &format!("let {n}=unsafe{{input({n},{n}_len)}}?;");
                    if t == "&str" {
                        init += &format!(
                            "let {n}=std::str::from_utf8({n}).map_err(|e|e.to_string())?;"
                        );
                    }
                } else if t == "&[f32]" || t == "&mut[f32]" {
                    let mutable = t == "&mut[f32]";
                    decl.push(format!(
                        "{n}:*{} f32,{n}_len:usize",
                        if mutable { "mut" } else { "const" }
                    ));
                    init += &format!(
                        "let {n}=unsafe{{{}({n},{n}_len)}}?;",
                        if mutable {
                            "float_output"
                        } else {
                            "float_input"
                        }
                    );
                } else if t.starts_with('&') {
                    let mutable = t.starts_with("&mut");
                    let ty = t.trim_start_matches('&').trim_start_matches("mut");
                    decl.push(format!(
                        "{n}:*{} {ty}",
                        if mutable { "mut" } else { "const" }
                    ));
                    init += &format!(
                        "let {n}=unsafe{{({n} as *{} crate::api::{ty}).as_{}()}}.ok_or(\"null {n}\")?;",
                        if mutable { "mut" } else { "const" },
                        if mutable { "mut" } else { "ref" }
                    );
                } else {
                    decl.push(format!("{n}:{}", t.replace("&mut", "&mut ")));
                }
                callargs.push(n);
            }
            let cty = if opaque.contains(&t) {
                format!("*mut {t}")
            } else if t == "String" || t == "Vec<u8>" {
                "*mut TKBuffer".into()
            } else {
                t.clone()
            };
            if t != "()" {
                decl.push(format!("out:*mut {cty}"));
                init += "if out.is_null(){return Err(\"null output\".into());}";
            }
            decl.push("error:*mut *mut TKBuffer".into());
            let call = format!(
                "crate::api::{name}({}){}",
                callargs.join(","),
                if result { "?" } else { "" }
            );
            let value = if opaque.contains(&t) {
                format!("Box::into_raw(Box::new({call})).cast::<{t}>()")
            } else if t == "String" {
                format!("buffer({call}.into_bytes())")
            } else if t == "Vec<u8>" {
                format!("buffer({call})")
            } else if records.contains_key(&t) {
                format!(
                    "{{let value={call};{t}{{{}}}}}",
                    records[&t]
                        .iter()
                        .map(|(n, _)| format!("{n}:value.{n},"))
                        .collect::<String>()
                )
            } else {
                call
            };
            let statement = if t == "()" {
                format!("{value};")
            } else {
                format!("let value={value};unsafe{{out.write(value);}}")
            };
            c += &format!(
                "#[unsafe(no_mangle)] pub unsafe extern \"C\" fn tidkod_{name}({})->i32{{let call=||{{{init}{statement}Ok(())}};unsafe{{invoke(error,call)}}}}\n",
                decl.join(",")
            );
        }
        let file = out.join("c.rs");
        fs::write(&file, c).unwrap();
        let config = cbindgen::Config {
            language: cbindgen::Language::C,
            include_guard: Some("TIDKOD_H".into()),
            cpp_compat: true,
            ..Default::default()
        };
        cbindgen::Builder::new()
            .with_config(config)
            .with_src(file)
            .with_src("src/c_support.rs")
            .generate()
            .expect("C header")
            .write_to_file(out.join("tidkod.h"));
    }
    // Stable, relocatable generated-source directory beside the Cargo artifacts.
    // Cargo supports both build/<package-hash>/out and build/<package>/<hash>/out.
    // Locate the profile directory without depending on that internal nesting depth.
    let profile = out
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
        .and_then(|dir| dir.parent())
        .expect("Cargo OUT_DIR must be beneath the profile's build directory");
    let generated = profile
        .join("tidkod-generated")
        .join(if native { "native" } else { "core" });
    fs::create_dir_all(&generated).unwrap();
    fs::copy(out.join("API.txt"), generated.join("API.txt")).unwrap();
    if env::var_os("CARGO_FEATURE_C").is_some() {
        fs::copy(out.join("tidkod.h"), generated.join("tidkod.h")).unwrap();
    }
    if env::var_os("CARGO_FEATURE_SWIFT").is_some() {
        for (from, to) in [
            ("swift/SwiftBridgeCore.swift", "SwiftBridgeCore.swift"),
            ("swift/SwiftBridgeCore.h", "SwiftBridgeCore.h"),
            ("swift/Tidkod/Tidkod.swift", "Tidkod.swift"),
            ("swift/Tidkod/Tidkod.h", "TidkodSwift.h"),
        ] {
            fs::copy(out.join(from), generated.join(to)).unwrap();
        }
        fs::write(
            generated.join("BridgingHeader.h"),
            "#include \"SwiftBridgeCore.h\"\n#include \"TidkodSwift.h\"\n",
        )
        .unwrap();
    }
    wrappers::generate(&generated, &opaque, &records, &functions);
    if env::var_os("CARGO_FEATURE_CSHARP").is_some() {
        csbindgen::Builder::default()
            .input_extern_file(out.join("c.rs"))
            .input_extern_file("src/c_support.rs")
            .csharp_namespace("Tidkod.Sys")
            .csharp_dll_name("tidkod_bindings")
            .generate_csharp_file(generated.join("NativeMethods.g.cs"))
            .expect("C# sys bindings");
        csharp::generate(&generated, &opaque, &records, &functions);
    }
    fs::write(generated.join("target.txt"), env::var("TARGET").unwrap()).unwrap();
    println!("cargo:metadata=generated={}", generated.display());
}
