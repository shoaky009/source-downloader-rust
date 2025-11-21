use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{DeriveInput, Path, parse_macro_input};

#[proc_macro_derive(SdComponent, attributes(component))]
pub fn derive_component(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, attrs, .. } = parse_macro_input!(input);

    let component_attr = attrs
        .iter()
        .find(|a| a.path().is_ident("component"))
        .expect("missing #[component(...)]");

    let trait_paths: Punctuated<Path, Comma> = component_attr
        .parse_args_with(Punctuated::parse_terminated)
        .expect("invalid attribute syntax");

    let methods = trait_paths.iter().map(|path| {
        // 获取 Trait 名称，例如 Source
        // 注意：如果你的 Trait 写法是 sdk::Source，这里建议用 segments.last() 来获取 Source 这个名字用于生成方法名
        let trait_ident = &path.segments.last().unwrap().ident;

        // 生成方法名，例如 as_source
        let method_name = format_ident!("as_{}", trait_ident.to_string().to_lowercase());

        quote! {
            // 修改点 1: 参数改为 self: std::sync::Arc<Self>
            // 修改点 2: 返回值改为 Option<std::sync::Arc<dyn Trait>>
            fn #method_name(self: std::sync::Arc<Self>) -> Option<std::sync::Arc<dyn #path>> {
                // 修改点 3: 直接返回 Some(self)。
                // Rust 编译器会自动将 Arc<Struct> 强转(Coerce)为 Arc<dyn Trait>，
                // 前提是 #ident 确实实现了 #path 指向的 Trait。
                Some(self)
            }
        }
    });

    quote! {
        impl sdk::component::SdComponent for #ident {
            #(#methods)*
        }
    }
        .into()
}
