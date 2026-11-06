//! Save every field. Runtime exclusions must explain how they are recreated.
use proc_macro::TokenStream;
use quote::{ToTokens, format_ident, quote};
use syn::{Data, DeriveInput, Fields, Type, parse_macro_input, parse_quote};

#[proc_macro_attribute]
pub fn state(args: TokenStream, input: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return syn::Error::new(proc_macro2::Span::call_site(), "checkpoint::state takes no arguments")
            .into_compile_error()
            .into();
    }
    let mut item = parse_macro_input!(input as DeriveInput);
    match expand(&mut item) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.into_compile_error().into(),
    }
}

fn expand(item: &mut DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(&item.generics, "checkpoint state requires a concrete owned type"));
    }
    let name = item.ident.clone();
    let helper_name = format_ident!("__Checkpoint{}", name);
    let mut helper = item.clone();
    helper.ident = helper_name.clone();
    helper.vis = syn::Visibility::Inherited;
    helper.attrs.clear();
    helper.attrs.push(parse_quote!(#[derive(checkpoint::serde::Deserialize)]));
    helper.attrs.push(parse_quote!(#[serde(crate = "checkpoint::serde", deny_unknown_fields)]));
    let body = match (&mut item.data, &mut helper.data) {
        (Data::Struct(a), Data::Struct(b)) => fields(&mut a.fields, &mut b.fields, quote!(Self), quote!(saved))?,
        (Data::Enum(a), Data::Enum(b)) => {
            let mut arms = Vec::new();
            for (a, b) in a.variants.iter_mut().zip(&mut b.variants) {
                let v = &a.ident;
                let names: Vec<_> = (0..a.fields.len()).map(|i| format_ident!("f{i}")).collect();
                let pattern = match &a.fields {
                    Fields::Named(f) => {
                        let ids = f.named.iter().map(|f| &f.ident);
                        quote!(#helper_name::#v { #(#ids: #names),* })
                    }
                    Fields::Unnamed(_) => quote!(#helper_name::#v(#(#names),*)),
                    Fields::Unit => quote!(#helper_name::#v),
                };
                let expressions =
                    field_exprs(&mut a.fields, &mut b.fields, &names.iter().map(|n| quote!(#n)).collect::<Vec<_>>())?;
                let result = construct(&a.fields, quote!(Self::#v), expressions);
                arms.push(quote!(#pattern => #result));
            }
            quote!(match saved { #(#arms),* })
        }
        _ => return Err(syn::Error::new_spanned(&*item, "unions cannot be checkpointed")),
    };
    item.attrs.push(parse_quote!(#[derive(checkpoint::serde::Serialize)]));
    item.attrs.push(parse_quote!(#[serde(crate = "checkpoint::serde")]));
    Ok(quote! {
        #item
        const _: () = {
            #helper
            impl<'de> checkpoint::serde::Deserialize<'de> for #name {
                fn deserialize<D: checkpoint::serde::Deserializer<'de>>(d: D) -> ::std::result::Result<Self, D::Error> {
                    let saved = <#helper_name as checkpoint::serde::Deserialize>::deserialize(d)?;
                    Ok(#body)
                }
            }
            impl checkpoint::StaticField for #name {
                type Owned = Self;
                fn resolve(value: Self) -> ::std::result::Result<Self, String> { Ok(value) }
            }
        };
    })
}
fn fields(
    a: &mut Fields,
    b: &mut Fields,
    prefix: proc_macro2::TokenStream,
    source: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let accesses = a
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let m =
                f.ident.clone().map(syn::Member::Named).unwrap_or_else(|| syn::Member::Unnamed(syn::Index::from(i)));
            quote!(#source.#m)
        })
        .collect::<Vec<_>>();
    let values = field_exprs(a, b, &accesses)?;
    Ok(construct(a, prefix, values))
}
fn construct(
    fields: &Fields,
    prefix: proc_macro2::TokenStream,
    values: Vec<proc_macro2::TokenStream>,
) -> proc_macro2::TokenStream {
    match fields {
        Fields::Named(f) => {
            let ids = f.named.iter().map(|f| &f.ident);
            quote!(#prefix { #(#ids: #values),* })
        }
        Fields::Unnamed(_) => quote!(#prefix(#(#values),*)),
        Fields::Unit => prefix,
    }
}
fn field_exprs(
    a: &mut Fields,
    b: &mut Fields,
    accesses: &[proc_macro2::TokenStream],
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    let mut out = Vec::new();
    for ((a, b), access) in a.iter_mut().zip(b.iter_mut()).zip(accesses) {
        let mut reset: Option<syn::Expr> = None;
        let mut reason = false;
        for attr in &a.attrs {
            if attr.path().is_ident("checkpoint") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rebuild") {
                        reset = Some(meta.value()?.parse::<syn::LitStr>()?.parse()?);
                        Ok(())
                    } else if meta.path.is_ident("reason") {
                        let s = meta.value()?.parse::<syn::LitStr>()?;
                        reason = !s.value().trim().is_empty();
                        Ok(())
                    } else {
                        Err(meta.error("expected rebuild = \"expression\", reason = \"why safe\""))
                    }
                })?;
            }
        }
        a.attrs.retain(|x| !x.path().is_ident("checkpoint"));
        b.attrs.retain(|x| !x.path().is_ident("checkpoint"));
        if reset.is_none() {
            let mut custom_deserializer = false;
            let mut custom_serializer = false;
            for attr in &a.attrs {
                if attr.path().is_ident("serde") {
                    attr.parse_nested_meta(|meta| {
                        if meta.path.is_ident("skip") || meta.path.is_ident("skip_serializing") || meta.path.is_ident("skip_deserializing") || meta.path.is_ident("default") || meta.path.is_ident("skip_serializing_if") {
                            return Err(meta.error("persistent fields cannot be skipped or defaulted; use checkpoint(rebuild, reason) for runtime resources"));
                        }
                        if meta.path.is_ident("serialize_with") || meta.path.is_ident("with") { custom_serializer = true; }
                        if meta.path.is_ident("deserialize_with") || meta.path.is_ident("with") { custom_deserializer = true; }
                        if meta.input.peek(syn::Token![=]) { let _: syn::Expr = meta.value()?.parse()?; }
                        Ok(())
                    })?;
                }
            }
            if !custom_serializer
                && a.ty
                    .to_token_stream()
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|word| matches!(word, "u128" | "i128"))
            {
                return Err(syn::Error::new_spanned(
                    &a.ty,
                    "checkpoint integers are at most 64 bits; use a bounded integer or an explicit portable serde field codec",
                ));
            }
            if !custom_deserializer {
                b.attrs.push(parse_quote!(#[serde(deserialize_with = "checkpoint::required")]));
            }
        }
        if let Some(expr) = reset {
            if !reason {
                return Err(syn::Error::new_spanned(&*a, "checkpoint rebuild requires a nonempty reason"));
            }
            a.attrs.push(parse_quote!(#[serde(skip_serializing)]));
            b.ty = parse_quote!(());
            b.attrs.push(parse_quote!(#[serde(skip)]));
            out.push(quote!(#expr));
        } else if a.ty.to_token_stream().to_string().contains("'static") {
            let ty: Type = a.ty.clone();
            b.ty = parse_quote!(<#ty as checkpoint::StaticField>::Owned);
            out.push(quote!(<#ty as checkpoint::StaticField>::resolve(#access).map_err(checkpoint::serde::de::Error::custom)?));
        } else {
            out.push(access.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsupported_integer_widths_even_inside_optional_state() {
        let mut item: DeriveInput = parse_quote!(
            struct Bad {
                cached: Option<(String, u128)>,
            }
        );
        assert!(expand(&mut item).unwrap_err().to_string().contains("at most 64 bits"));
    }
    #[test]
    fn runtime_exclusions_require_a_reason() {
        let mut item: DeriveInput = parse_quote!(
            struct Bad {
                #[checkpoint(rebuild = "None")]
                socket: Option<String>,
            }
        );
        assert!(expand(&mut item).unwrap_err().to_string().contains("reason"));
    }
}
