//! Effective default Python token styles of the built-in Dark, Light and High Contrast schemes,
//! with attribute inheritance already resolved.
use crate::{ColorScheme, HighlightTag as T, Rgb, TextStyle};
pub(crate) fn apply(scheme: &mut ColorScheme, profile: &str) {
    let styles: &[(T, TextStyle)] = match profile {
        "Dark" => &[
            (
                T::Keyword,
                TextStyle {
                    fg: Some(Rgb::hex(0xcc7832)),
                    ..Default::default()
                },
            ),
            (
                T::Bytes,
                TextStyle {
                    fg: Some(Rgb::hex(0x6a8759)),
                    ..Default::default()
                },
            ),
            (
                T::String,
                TextStyle {
                    fg: Some(Rgb::hex(0x6a8759)),
                    ..Default::default()
                },
            ),
            (
                T::Number,
                TextStyle {
                    fg: Some(Rgb::hex(0x6897bb)),
                    ..Default::default()
                },
            ),
            (
                T::Operator,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::Parenthesis,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::Bracket,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::Brace,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::Comma,
                TextStyle {
                    fg: Some(Rgb::hex(0xcc7832)),
                    ..Default::default()
                },
            ),
            (
                T::Dot,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::Comment,
                TextStyle {
                    fg: Some(Rgb::hex(0x808080)),
                    ..Default::default()
                },
            ),
            (
                T::Docstring,
                TextStyle {
                    fg: Some(Rgb::hex(0x629755)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::DocCommentTag,
                TextStyle {
                    fg: Some(Rgb::hex(0x629755)),
                    effect_color: Some(Rgb::hex(0x629755)),
                    bold: true,
                    italic: true,
                    underline: true,
                    ..Default::default()
                },
            ),
            (
                T::Decorator,
                TextStyle {
                    fg: Some(Rgb::hex(0xbbb529)),
                    ..Default::default()
                },
            ),
            (
                T::ClassDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::FunctionDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xffc66d)),
                    ..Default::default()
                },
            ),
            (
                T::NestedFunctionDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xffc66d)),
                    ..Default::default()
                },
            ),
            (
                T::PredefinedDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::PredefinedUsage,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Builtin,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Parameter,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::SelfParameter,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::KeywordArgument,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::FunctionCall,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::MethodCall,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::TypeAnnotation,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::EscapeSequence,
                TextStyle {
                    fg: Some(Rgb::hex(0xcc7832)),
                    ..Default::default()
                },
            ),
            (
                T::InvalidEscapeSequence,
                TextStyle {
                    fg: Some(Rgb::hex(0x6a8759)),
                    effect_color: Some(Rgb::hex(0xff0000)),
                    wave: true,
                    ..Default::default()
                },
            ),
            (
                T::FStringBrace,
                TextStyle {
                    fg: Some(Rgb::hex(0xcc7832)),
                    ..Default::default()
                },
            ),
            (
                T::FStringColon,
                TextStyle {
                    fg: Some(Rgb::hex(0xcc7832)),
                    ..Default::default()
                },
            ),
            (
                T::FStringConversion,
                TextStyle {
                    fg: Some(Rgb::hex(0xcc7832)),
                    ..Default::default()
                },
            ),
            (
                T::FStringFormatCharacter,
                TextStyle {
                    fg: Some(Rgb::hex(0xcc7832)),
                    ..Default::default()
                },
            ),
            (
                T::FStringFormatNumber,
                TextStyle {
                    fg: Some(Rgb::hex(0x6897bb)),
                    ..Default::default()
                },
            ),
            (
                T::LocalVariable,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
            (
                T::TypeParameter,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b7c6)),
                    ..Default::default()
                },
            ),
        ],
        "Light" => &[
            (
                T::Keyword,
                TextStyle {
                    fg: Some(Rgb::hex(0x000080)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::Bytes,
                TextStyle {
                    fg: Some(Rgb::hex(0x008000)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::String,
                TextStyle {
                    fg: Some(Rgb::hex(0x008000)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::Number,
                TextStyle {
                    fg: Some(Rgb::hex(0x0000ff)),
                    ..Default::default()
                },
            ),
            (
                T::Operator,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::Parenthesis,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::Bracket,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::Brace,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::Comma,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::Dot,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::Comment,
                TextStyle {
                    fg: Some(Rgb::hex(0x808080)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Docstring,
                TextStyle {
                    fg: Some(Rgb::hex(0x808080)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::DocCommentTag,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    effect_color: Some(Rgb::hex(0x808080)),
                    bold: true,
                    underline: true,
                    ..Default::default()
                },
            ),
            (
                T::Decorator,
                TextStyle {
                    fg: Some(Rgb::hex(0x808000)),
                    ..Default::default()
                },
            ),
            (
                T::ClassDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::FunctionDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::NestedFunctionDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::PredefinedDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::PredefinedUsage,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Builtin,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Parameter,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::SelfParameter,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::KeywordArgument,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::FunctionCall,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::MethodCall,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::TypeAnnotation,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::EscapeSequence,
                TextStyle {
                    fg: Some(Rgb::hex(0x000080)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::InvalidEscapeSequence,
                TextStyle {
                    fg: Some(Rgb::hex(0x008000)),
                    bg: Some(Rgb::hex(0xffcccc)),
                    ..Default::default()
                },
            ),
            (
                T::FStringBrace,
                TextStyle {
                    fg: Some(Rgb::hex(0x000080)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::FStringColon,
                TextStyle {
                    fg: Some(Rgb::hex(0x000080)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::FStringConversion,
                TextStyle {
                    fg: Some(Rgb::hex(0x000080)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::FStringFormatCharacter,
                TextStyle {
                    fg: Some(Rgb::hex(0x000080)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::FStringFormatNumber,
                TextStyle {
                    fg: Some(Rgb::hex(0x0000ff)),
                    ..Default::default()
                },
            ),
            (
                T::LocalVariable,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
            (
                T::TypeParameter,
                TextStyle {
                    fg: Some(Rgb::hex(0x000000)),
                    ..Default::default()
                },
            ),
        ],
        "High Contrast" => &[
            (
                T::Keyword,
                TextStyle {
                    fg: Some(Rgb::hex(0xed864a)),
                    ..Default::default()
                },
            ),
            (
                T::Bytes,
                TextStyle {
                    fg: Some(Rgb::hex(0x54b33e)),
                    ..Default::default()
                },
            ),
            (
                T::String,
                TextStyle {
                    fg: Some(Rgb::hex(0x54b33e)),
                    ..Default::default()
                },
            ),
            (
                T::Number,
                TextStyle {
                    fg: Some(Rgb::hex(0x33ccff)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::Operator,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    ..Default::default()
                },
            ),
            (
                T::Parenthesis,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    ..Default::default()
                },
            ),
            (
                T::Bracket,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    ..Default::default()
                },
            ),
            (
                T::Brace,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    ..Default::default()
                },
            ),
            (
                T::Comma,
                TextStyle {
                    fg: Some(Rgb::hex(0xed864a)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::Dot,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    ..Default::default()
                },
            ),
            (
                T::Comment,
                TextStyle {
                    fg: Some(Rgb::hex(0x7ec3e6)),
                    ..Default::default()
                },
            ),
            (
                T::Docstring,
                TextStyle {
                    fg: Some(Rgb::hex(0x499936)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::DocCommentTag,
                TextStyle {
                    fg: Some(Rgb::hex(0x499936)),
                    effect_color: Some(Rgb::hex(0x629755)),
                    bold: true,
                    italic: true,
                    underline: true,
                    ..Default::default()
                },
            ),
            (
                T::Decorator,
                TextStyle {
                    fg: Some(Rgb::hex(0xa9b837)),
                    ..Default::default()
                },
            ),
            (
                T::ClassDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xffffff)),
                    ..Default::default()
                },
            ),
            (
                T::FunctionDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xffcf40)),
                    ..Default::default()
                },
            ),
            (
                T::NestedFunctionDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xffcf40)),
                    ..Default::default()
                },
            ),
            (
                T::PredefinedDefinition,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::PredefinedUsage,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Builtin,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Parameter,
                TextStyle {
                    fg: Some(Rgb::hex(0xffffff)),
                    ..Default::default()
                },
            ),
            (
                T::SelfParameter,
                TextStyle {
                    fg: Some(Rgb::hex(0xffffff)),
                    ..Default::default()
                },
            ),
            (
                T::KeywordArgument,
                TextStyle {
                    fg: Some(Rgb::hex(0xffffff)),
                    ..Default::default()
                },
            ),
            (
                T::FunctionCall,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    ..Default::default()
                },
            ),
            (
                T::MethodCall,
                TextStyle {
                    fg: Some(Rgb::hex(0xebebeb)),
                    ..Default::default()
                },
            ),
            (
                T::TypeAnnotation,
                TextStyle {
                    fg: Some(Rgb::hex(0xffffff)),
                    ..Default::default()
                },
            ),
            (
                T::EscapeSequence,
                TextStyle {
                    fg: Some(Rgb::hex(0xed864a)),
                    ..Default::default()
                },
            ),
            (
                T::InvalidEscapeSequence,
                TextStyle {
                    fg: Some(Rgb::hex(0x6a8759)),
                    effect_color: Some(Rgb::hex(0xff0d25)),
                    bold_underline: true,
                    ..Default::default()
                },
            ),
            (
                T::FStringBrace,
                TextStyle {
                    fg: Some(Rgb::hex(0xed864a)),
                    ..Default::default()
                },
            ),
            (
                T::FStringColon,
                TextStyle {
                    fg: Some(Rgb::hex(0xed864a)),
                    ..Default::default()
                },
            ),
            (
                T::FStringConversion,
                TextStyle {
                    fg: Some(Rgb::hex(0xed864a)),
                    ..Default::default()
                },
            ),
            (
                T::FStringFormatCharacter,
                TextStyle {
                    fg: Some(Rgb::hex(0xed864a)),
                    ..Default::default()
                },
            ),
            (
                T::FStringFormatNumber,
                TextStyle {
                    fg: Some(Rgb::hex(0x33ccff)),
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                T::LocalVariable,
                TextStyle {
                    fg: Some(Rgb::hex(0xffffff)),
                    ..Default::default()
                },
            ),
            (
                T::TypeParameter,
                TextStyle {
                    fg: Some(Rgb::hex(0xffffff)),
                    ..Default::default()
                },
            ),
        ],
        _ => return,
    };
    for (tag, style) in styles {
        scheme.styles.insert(*tag, *style);
    }
    scheme.styles.insert(T::FString, scheme.style(T::String));
    scheme
        .styles
        .insert(T::SoftKeyword, scheme.style(T::Keyword));
}
