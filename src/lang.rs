use tree_sitter::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Rust,
    Go,
    Java,
    CSharp,
    C,
    Cpp,
    Php,
    Ruby,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Python,
    Ts,
    Rust,
    Go,
    Java,
    CSharp,
    C,
    Php,
    Ruby,
}

impl Lang {
    pub fn from_path(path: &str) -> Option<Lang> {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".d.ts") || lower.ends_with(".min.js") {
            return None;
        }
        let ext = lower.rsplit_once('.')?.1;
        match ext {
            "py" | "pyi" => Some(Lang::Python),
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
            "rs" => Some(Lang::Rust),
            "go" => Some(Lang::Go),
            "java" if cfg!(feature = "lang-java") => Some(Lang::Java),
            "cs" if cfg!(feature = "lang-csharp") => Some(Lang::CSharp),
            "c" if cfg!(feature = "lang-c") => Some(Lang::C),
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "h" if cfg!(feature = "lang-cpp") => {
                Some(Lang::Cpp)
            }
            "h" if cfg!(feature = "lang-c") => Some(Lang::C),
            "php" if cfg!(feature = "lang-php") => Some(Lang::Php),
            "rb" if cfg!(feature = "lang-ruby") => Some(Lang::Ruby),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Rust => "rust",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::CSharp => "csharp",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Php => "php",
            Lang::Ruby => "ruby",
        }
    }

    pub fn family(self) -> Family {
        match self {
            Lang::Python => Family::Python,
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => Family::Ts,
            Lang::Rust => Family::Rust,
            Lang::Go => Family::Go,
            Lang::Java => Family::Java,
            Lang::CSharp => Family::CSharp,
            Lang::C | Lang::Cpp => Family::C,
            Lang::Php => Family::Php,
            Lang::Ruby => Family::Ruby,
        }
    }

    /// Languages where several methods can share a name and differ only by parameters.
    pub fn overloadable(self) -> bool {
        matches!(self, Lang::Java | Lang::CSharp | Lang::C | Lang::Cpp)
    }

    pub fn ts_language(self) -> Language {
        match self {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Java => java_lang(),
            Lang::CSharp => csharp_lang(),
            Lang::C => c_lang(),
            Lang::Cpp => cpp_lang(),
            Lang::Php => php_lang(),
            Lang::Ruby => ruby_lang(),
        }
    }
}

#[cfg(feature = "lang-java")]
fn java_lang() -> Language {
    tree_sitter_java::LANGUAGE.into()
}
#[cfg(not(feature = "lang-java"))]
fn java_lang() -> Language {
    unreachable!("java support disabled at build time")
}

#[cfg(feature = "lang-csharp")]
fn csharp_lang() -> Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}
#[cfg(not(feature = "lang-csharp"))]
fn csharp_lang() -> Language {
    unreachable!("c# support disabled at build time")
}

#[cfg(feature = "lang-c")]
fn c_lang() -> Language {
    tree_sitter_c::LANGUAGE.into()
}
#[cfg(not(feature = "lang-c"))]
fn c_lang() -> Language {
    unreachable!("c support disabled at build time")
}

#[cfg(feature = "lang-cpp")]
fn cpp_lang() -> Language {
    tree_sitter_cpp::LANGUAGE.into()
}
#[cfg(not(feature = "lang-cpp"))]
fn cpp_lang() -> Language {
    unreachable!("c++ support disabled at build time")
}

#[cfg(feature = "lang-php")]
fn php_lang() -> Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}
#[cfg(not(feature = "lang-php"))]
fn php_lang() -> Language {
    unreachable!("php support disabled at build time")
}

#[cfg(feature = "lang-ruby")]
fn ruby_lang() -> Language {
    tree_sitter_ruby::LANGUAGE.into()
}
#[cfg(not(feature = "lang-ruby"))]
fn ruby_lang() -> Language {
    unreachable!("ruby support disabled at build time")
}
