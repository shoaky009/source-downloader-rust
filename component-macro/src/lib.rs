use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{parse_macro_input, DeriveInput, Path};

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
        let trait_ident = &path.segments.last().unwrap().ident;
        let trait_name = trait_ident.to_string();
        let method_name = format_ident!("as_{}", to_snake_case(trait_ident.to_string().as_str()));
        if trait_name == "Stateful" {
            quote! {
                fn #method_name(self: std::sync::Arc<Self>) -> Option<std::sync::Arc<dyn #path>> {
                    Some(self)
                }
            }
        } else {
            quote! {
                fn #method_name(self: std::sync::Arc<Self>) -> Result<std::sync::Arc<dyn #path>, source_downloader_sdk::component::ComponentError> {
                    Ok(self)
                }
            }
        }
    });

    quote! {
        impl source_downloader_sdk::component::SdComponent for #ident {
            #(#methods)*
        }
    }
    .into()
}

fn to_snake_case(input: &str) -> String {
    let mut out = String::new();
    for (i, ch) in input.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
