//! C++ ownership wrappers over the exported C ABI; no C++ runtime crosses the ABI.
use super::{
    args, output,
    wrappers::{base, scalar, snake},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub fn generate(
    dir: &Path,
    opaque: &BTreeSet<String>,
    records: &BTreeMap<String, Vec<(String, String)>>,
    functions: &[syn::ItemFn],
) {
    let mut header = String::from(
        r#"// Generated; do not edit.
#pragma once
#include "tidkod.h"
#include <optional>
#include <string>
#include <string_view>
#include <vector>
#include <utility>
#include <cstdlib>
namespace tidkod::client {
struct FloatInput {const float* data; size_t size;};
struct FloatOutput {float* data; size_t size;};
template<class T> class Result {
    std::optional<T> value_; std::string error_;
    Result(std::string error, int): error_(std::move(error)) {}
public:
    explicit Result(T value): value_(std::move(value)) {}
    static Result failure(std::string error) { return Result(std::move(error), 0); }
    explicit operator bool() const { return value_.has_value(); }
    const std::string& error() const { return error_; }
    T& value() { if (!value_) std::abort(); return *value_; }
    T take() { if (!value_) std::abort(); T result=std::move(*value_); value_.reset(); return result; }
};
template<> class Result<void> {
    bool ok_=true; std::string error_;
public:
    static Result failure(std::string error) { Result r; r.ok_=false; r.error_=std::move(error); return r; }
    explicit operator bool() const { return ok_; }
    const std::string& error() const { return error_; }
};
namespace detail {
struct Buffer {
    ::TKBuffer* raw=nullptr;
    ~Buffer() { tidkod_buffer_free(raw); }
    Buffer()=default;
    Buffer(const Buffer&)=delete;
    Buffer& operator=(const Buffer&)=delete;
    std::string text() const {
        auto size=tidkod_buffer_len(raw);
        return size ? std::string(reinterpret_cast<const char*>(tidkod_buffer_data(raw)), size) : std::string();
    }
    std::vector<uint8_t> bytes() const {
        auto size=tidkod_buffer_len(raw);
        auto data=tidkod_buffer_data(raw);
        return size ? std::vector<uint8_t>(data, data+size) : std::vector<uint8_t>();
    }
};
}
"#,
    );
    for name in opaque {
        header += &format!("class {name};\n");
    }
    for name in records.keys() {
        header += &format!("using {name} = ::{name};\n");
    }
    let typ = |t: &str| match t {
        "String" => "std::string".into(),
        "&str" => "std::string_view".into(),
        "Vec<u8>" => "std::vector<uint8_t>".into(),
        "&[u8]" => "const std::vector<uint8_t>&".into(),
        "&[f32]" => "FloatInput".into(),
        "&mut[f32]" => "FloatOutput".into(),
        _ if t.starts_with('&') => format!(
            "{}{}&",
            if t.starts_with("&mut") { "" } else { "const " },
            base(t)
        ),
        _ => scalar(t, false),
    };
    let mut bodies = String::new();
    for owner in opaque {
        let free = format!("tidkod_{}_free", owner.to_lowercase());
        header += &format!(
            r#"class {owner} {{
    ::{owner}* raw_;
public:
    explicit {owner}(::{owner}* raw):raw_(raw) {{}}
    ~{owner}() {{ {free}(raw_); }}
    {owner}({owner}&& other) noexcept:raw_(std::exchange(other.raw_,nullptr)) {{}}
    {owner}& operator=({owner}&& other) noexcept {{
        if(this!=&other) {{ {free}(raw_); raw_=std::exchange(other.raw_,nullptr); }}
        return *this;
    }}
    {owner}(const {owner}&)=delete;
    {owner}& operator=(const {owner}&)=delete;
    ::{owner}* raw() const {{ return raw_; }}
"#
        );
        for f in functions {
            let name = f.sig.ident.to_string();
            let owning = opaque
                .iter()
                .filter(|t| name.starts_with(&(snake(t) + "_")))
                .max_by_key(|t| t.len())
                .unwrap();
            if owning != owner {
                continue;
            }
            let method = name.strip_prefix(&(snake(owner) + "_")).unwrap();
            let method = if method == "new" { "create" } else { method };
            let a = args(f);
            let instance = a
                .first()
                .is_some_and(|(_, t)| t.starts_with('&') && base(t) == owner);
            let (ret, fallible) = output(f);
            let qualifier = if instance && !a[0].1.starts_with("&mut") {
                " const"
            } else {
                ""
            };
            let rt = if fallible {
                format!("Result<{}>", typ(&ret))
            } else {
                typ(&ret)
            };
            let decl = a[usize::from(instance)..]
                .iter()
                .map(|(n, t)| format!("{} {n}", typ(t)))
                .collect::<Vec<_>>()
                .join(", ");
            header += &format!(
                "{} {rt} {method}({decl}){qualifier};\n",
                if instance { "" } else { "static" }
            );
            bodies += &format!(
                "inline {rt} {owner}::{method}({decl}){qualifier} {{\ndetail::Buffer error;\n"
            );
            let mut callargs = Vec::new();
            for (i, (n, t)) in a.iter().enumerate() {
                callargs.push(if instance && i == 0 {
                    "raw_".into()
                } else if opaque.contains(base(t)) {
                    format!("{n}.raw()")
                } else if t == "&str" {
                    format!("reinterpret_cast<const uint8_t*>({n}.data()), {n}.size()")
                } else if t == "&[f32]" || t == "&mut[f32]" {
                    format!("{n}.data, {n}.size")
                } else if t == "&[u8]" {
                    format!("{n}.data(), {n}.size()")
                } else {
                    n.clone()
                });
            }
            if ret != "()" {
                if ret == "String" || ret == "Vec<u8>" {
                    bodies += "detail::Buffer value;\n";
                    callargs.push("&value.raw".into());
                } else {
                    let ctype = if opaque.contains(&ret) {
                        format!("::{ret}*")
                    } else {
                        typ(&ret)
                    };
                    bodies += &format!("{ctype} value{{}};\n");
                    callargs.push("&value".into());
                }
            }
            callargs.push("&error.raw".into());
            bodies += &format!("auto status=::tidkod_{name}({});\n", callargs.join(","));
            if fallible {
                bodies += &format!("if(status) return {rt}::failure(error.text());\n");
            } else {
                bodies += "if(status) std::abort();\n";
            }
            if ret != "()" {
                let converted = if opaque.contains(&ret) {
                    format!("{ret}(value)")
                } else if ret == "String" {
                    "value.text()".into()
                } else if ret == "Vec<u8>" {
                    "value.bytes()".into()
                } else {
                    "value".into()
                };
                bodies += &format!(
                    "return {};\n",
                    if fallible {
                        format!("{rt}({converted})")
                    } else {
                        converted
                    }
                );
            } else if fallible {
                bodies += "return Result<void>();\n";
            }
            bodies += "}\n";
        }
        header += "};\n";
    }
    header += &bodies;
    header += "}\n";
    fs::write(dir.join("tidkod-client.hpp"), header).unwrap();
}
