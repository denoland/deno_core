use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use quote::ToTokens;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::parse2;
use syn::spanned::Spanned;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::Fields;
use syn::LitStr;
use syn::Meta;

pub(crate) fn js_error(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;

  let (class, get_message, out_properties) = match input.data {
    Data::Enum(data) => {
      let top_class_attr = input
        .attrs
        .into_iter()
        .find_map(|attr| ClassAttrValue::from_attribute(attr).transpose())
        .transpose()?;
      if let Some(top_class_attr) = &top_class_attr {
        if matches!(top_class_attr, ClassAttrValue::Inherit(_)) {
          return Err(Error::new(
            top_class_attr.span(),
            "top level class attribute cannot be inherit",
          ));
        }
      }

      let mut get_class = vec![];
      let mut get_message = vec![];
      let mut get_properties = vec![];

      for variant in data.variants {
        let class_attr = variant
          .attrs
          .into_iter()
          .find_map(|attr| ClassAttrValue::from_attribute(attr).transpose())
          .unwrap_or_else(|| {
            top_class_attr.clone().ok_or_else(|| {
              Error::new(variant.ident.span(), "class attribute is missing")
            })
          })?;

        let variant_ident = variant.ident;

        let properties =
          properties_to_tokens(get_properties_from_fields(&variant.fields)?);

        let inherit_member = match variant.fields {
          Fields::Named(fields_named) => fields_named
            .named
            .iter()
            .find(get_inherit_attr_field)
            .map(|field| syn::Member::Named(field.ident.clone().unwrap())),
          Fields::Unnamed(fields_unnamed) => fields_unnamed
            .unnamed
            .iter()
            .enumerate()
            .find(|(_, field)| get_inherit_attr_field(field))
            .map(|(i, _)| syn::Member::Unnamed(syn::Index::from(i))),
          Fields::Unit => None,
        };

        let match_arm = if let Some(member) = &inherit_member {
          quote!(Self::#variant_ident { #member: inherit, .. })
        } else {
          quote!(Self::#variant_ident { .. })
        };

        let class = if matches!(class_attr, ClassAttrValue::Inherit(_)) {
          if inherit_member.is_some() {
            quote!(::deno_core::error::JsErrorClass::get_class(inherit))
          } else {
            return Err(Error::new(
              class_attr.span(),
              "class attribute was set to inherit, but no field was marked as inherit",
            ));
          }
        } else {
          quote!(#class_attr)
        };
        get_class.push(quote! {
          #match_arm => #class,
        });

        let message = if inherit_member.is_some() {
          quote!(::deno_core::error::JsErrorClass::get_message(inherit))
        } else {
          quote!(self.to_string().into())
        };
        get_message.push(quote! {
          #match_arm => #message,
        });

        // TODO: handle adding properties in addition to inherited ones
        let properties = if inherit_member.is_some() {
          quote!(::deno_core::error::JsErrorClass::get_additional_properties(
            inherit
          ))
        } else {
          properties
        };
        get_properties.push(quote! {
          #match_arm => {
            #properties
          }
        });
      }

      (
        quote! {
          match self {
            #(#get_class)*
          }
        },
        quote! {
          match self {
            #(#get_message)*
          }
        },
        quote! {
          match self {
            #(#get_properties)*
          }
        },
      )
    }
    Data::Struct(data) => {
      todo!();
      let class = input
        .attrs
        .into_iter()
        .find_map(|attr| ClassAttrValue::from_attribute(attr).transpose())
        .transpose()?
        .ok_or_else(|| {
          Error::new(input.ident.span(), "class attribute is missing")
        })?;
      let properties = get_properties_from_fields(&data.fields)?;

      let out_properties = properties_to_tokens(properties);

      (class.into_token_stream(), quote!(""), out_properties)
    }
    Data::Union(_) => {
      return Err(Error::new(input.span(), "Unions are not supported"))
    }
  };

  let ident = input.ident;

  Ok(quote! {
    #[allow(unused_qualifications)]
    impl ::deno_core::error::JsErrorClass for #ident {
      fn get_class(&self) -> &'static str {
        #class
      }
      fn get_message(&self) -> ::std::borrow::Cow<'static, str> {
        #get_message
      }
      fn get_additional_properties(
        &self
      ) -> Option<Vec<(::std::borrow::Cow<'static, str>, ::std::borrow::Cow<'static, str>)>> {
        #out_properties
      }
    }
  })
}

fn get_inherit_attr_field(field: &&syn::Field) -> bool {
  field
    .attrs
    .iter()
    .any(|attr| attr.path().is_ident("inherit"))
}

mod kw {
  syn::custom_keyword!(class);
  syn::custom_keyword!(property);
  syn::custom_keyword!(inherit);
}

#[derive(Debug, Clone)]
enum ClassAttrValue {
  Lit(syn::LitStr),
  Ident(Ident),
  Inherit(kw::inherit),
}

impl ClassAttrValue {
  fn from_attribute(attr: syn::Attribute) -> Result<Option<Self>, Error> {
    if attr.path().is_ident("class") {
      let list = attr.meta.require_list()?;
      let value = list.parse_args::<Self>()?;
      return Ok(Some(value));
    }

    Ok(None)
  }
}

impl ToTokens for ClassAttrValue {
  fn to_tokens(&self, tokens: &mut TokenStream) {
    let class_tokens = match self {
      ClassAttrValue::Lit(lit) => quote!(#lit),
      ClassAttrValue::Ident(ident) => {
        let error_name = format_ident!("{}_ERROR", ident);
        quote!(::deno_core::error::builtin_classes::#error_name)
      }
      ClassAttrValue::Inherit(_) => unreachable!(),
    };

    class_tokens.to_tokens(tokens)
  }
}

impl Parse for ClassAttrValue {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let lookahead = input.lookahead1();

    if lookahead.peek(syn::LitStr) {
      Ok(Self::Lit(input.parse()?))
    } else if lookahead.peek(kw::inherit) {
      Ok(Self::Inherit(input.parse()?))
    } else if lookahead.peek(syn::Ident) {
      Ok(Self::Ident(input.parse()?))
    } else {
      Err(lookahead.error())
    }
  }
}

#[derive(Debug)]
struct ParsedProperty {
  ident: syn::Member,
  name: String,
}

fn get_properties_from_fields(
  fields: &Fields,
) -> Result<Vec<ParsedProperty>, Error> {
  const PROPERTY_IDENT: &str = "property";
  let mut out_fields = vec![];

  match fields {
    Fields::Named(named) => {
      for field in &named.named {
        for attr in &field.attrs {
          if attr.path().is_ident(PROPERTY_IDENT) {
            let name = match &attr.meta {
              Meta::Path(_) => None,
              Meta::List(list) => {
                return Err(Error::new(
                  list.delimiter.span().open(),
                  "expected `=`",
                ));
              }
              Meta::NameValue(meta) => {
                Some(parse2::<LitStr>(meta.value.to_token_stream())?.value())
              }
            };

            let ident = field.ident.clone().unwrap();
            let name = name.unwrap_or_else(|| ident.to_string());
            let ident = syn::Member::Named(field.ident.clone().unwrap());
            out_fields.push(ParsedProperty { name, ident });

            break;
          }
        }
      }
    }
    Fields::Unnamed(unnamed) => {
      for (i, field) in unnamed.unnamed.iter().enumerate() {
        for attr in &field.attrs {
          if attr.path().is_ident(PROPERTY_IDENT) {
            let name_value = attr.meta.require_name_value()?;
            let name =
              parse2::<LitStr>(name_value.value.to_token_stream())?.value();

            let ident = syn::Member::Unnamed(syn::Index::from(i));
            out_fields.push(ParsedProperty { name, ident });

            break;
          }
        }
      }
    }
    Fields::Unit => {}
  }

  Ok(out_fields)
}

fn properties_to_tokens(properties: Vec<ParsedProperty>) -> TokenStream {
  if !properties.is_empty() {
    let properties = properties
      .into_iter()
      .map(|property| {
        let ident = property.ident;
        let ident_str = property.name;

        quote! {
          (
            ::std::borrow::Cow::Borrowed(#ident_str),
            ::std::borrow::Cow::Owned(self.#ident.to_string()),
          ),
        }
      })
      .collect::<Vec<_>>();

    quote! {
      Some(vec![
        #(#properties),*
      ])
    }
  } else {
    quote!(None)
  }
}
