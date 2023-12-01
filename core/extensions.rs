// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::modules::ModuleCode;
use crate::OpState;
use anyhow::Context as _;
use anyhow::Error;
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Context;
use v8::fast_api::FastFunction;
use v8::ExternalReference;

#[derive(Clone, Debug)]
pub enum ExtensionFileSourceCode {
  /// Source code is included in the binary produced. Either by being defined
  /// inline, or included using `include_str!()`. If you are snapshotting, this
  /// will result in two copies of the source code being included - one in the
  /// snapshot, the other the static string in the `Extension`.
  IncludedInBinary(&'static str),

  // Source code is loaded from a file on disk. It's meant to be used if the
  // embedder is creating snapshots. Files will be loaded from the filesystem
  // during the build time and they will only be present in the V8 snapshot.
  LoadedFromFsDuringSnapshot(&'static str), // <- Path

  /// Source code may be computed at runtime.
  Computed(Arc<str>),
}

#[derive(Clone, Debug)]
pub struct ExtensionFileSource {
  pub specifier: &'static str,
  pub code: ExtensionFileSourceCode,
}

impl ExtensionFileSource {
  fn find_non_ascii(s: &str) -> String {
    s.chars().filter(|c| !c.is_ascii()).collect::<String>()
  }

  pub fn load(&self) -> Result<ModuleCode, Error> {
    match &self.code {
      ExtensionFileSourceCode::IncludedInBinary(code) => {
        debug_assert!(
          code.is_ascii(),
          "Extension code must be 7-bit ASCII: {} (found {})",
          self.specifier,
          Self::find_non_ascii(code)
        );
        Ok(ModuleCode::from_static(code))
      }
      ExtensionFileSourceCode::LoadedFromFsDuringSnapshot(path) => {
        let msg = || format!("Failed to read \"{}\"", path);
        let s = std::fs::read_to_string(path).with_context(msg)?;
        debug_assert!(
          s.is_ascii(),
          "Extension code must be 7-bit ASCII: {} (found {})",
          self.specifier,
          Self::find_non_ascii(&s)
        );
        Ok(s.into())
      }
      ExtensionFileSourceCode::Computed(code) => {
        debug_assert!(
          code.is_ascii(),
          "Extension code must be 7-bit ASCII: {} (found {})",
          self.specifier,
          Self::find_non_ascii(code)
        );
        Ok(ModuleCode::Arc(code.clone()))
      }
    }
  }
}

pub type OpFnRef = v8::FunctionCallback;
pub type OpMiddlewareFn = dyn Fn(OpDecl) -> OpDecl;
pub type OpStateFn = dyn FnOnce(&mut OpState);
/// Trait implemented by all generated ops.
pub trait Op {
  const NAME: &'static str;
  const DECL: OpDecl;
}
pub type EventLoopMiddlewareFn = fn(Rc<RefCell<OpState>>, &mut Context) -> bool;
pub type GlobalTemplateMiddlewareFn =
  for<'s> fn(
    &mut v8::HandleScope<'s, ()>,
    v8::Local<'s, v8::ObjectTemplate>,
  ) -> v8::Local<'s, v8::ObjectTemplate>;
pub type GlobalObjectMiddlewareFn =
  for<'s> fn(&mut v8::HandleScope<'s>, v8::Local<'s, v8::Object>);

#[derive(Copy, Clone)]
pub struct OpDecl {
  pub name: &'static str,
  pub enabled: bool,
  pub is_async: bool,
  pub is_reentrant: bool,
  pub arg_count: u8,
  /// The slow dispatch call. If metrics are disabled, the `v8::Function` is created with this callback.
  pub(crate) slow_fn: OpFnRef,
  /// The slow dispatch call with metrics enabled. If metrics are enabled, the `v8::Function` is created with this callback.
  pub(crate) slow_fn_with_metrics: OpFnRef,
  /// The fast dispatch call. If metrics are disabled, the `v8::Function`'s fastcall is created with this callback.
  pub(crate) fast_fn: Option<FastFunction>,
  /// The fast dispatch call with metrics enabled. If metrics are enabled, the `v8::Function`'s fastcall is created with this callback.
  pub(crate) fast_fn_with_metrics: Option<FastFunction>,
}

impl OpDecl {
  /// For use by internal op implementation only.
  #[doc(hidden)]
  #[allow(clippy::too_many_arguments)]
  pub const fn new_internal_op2(
    name: &'static str,
    is_async: bool,
    is_reentrant: bool,
    arg_count: u8,
    slow_fn: OpFnRef,
    slow_fn_with_metrics: OpFnRef,
    fast_fn: Option<FastFunction>,
    fast_fn_with_metrics: Option<FastFunction>,
  ) -> Self {
    #[allow(deprecated)]
    Self {
      name,
      enabled: true,
      is_async,
      is_reentrant,
      arg_count,
      slow_fn,
      slow_fn_with_metrics,
      fast_fn,
      fast_fn_with_metrics,
    }
  }

  /// Returns a copy of this `OpDecl` with `enabled` set to the given state.
  pub const fn enabled(self, enabled: bool) -> Self {
    Self { enabled, ..self }
  }

  /// Returns a copy of this `OpDecl` with `enabled` set to `false`.
  pub const fn disable(self) -> Self {
    self.enabled(false)
  }

  /// Returns a copy of this `OpDecl` with the implementation function set to the function from another
  /// `OpDecl`.
  pub const fn with_implementation_from(self, from: &Self) -> Self {
    Self {
      slow_fn: from.slow_fn,
      slow_fn_with_metrics: from.slow_fn_with_metrics,
      fast_fn: from.fast_fn,
      fast_fn_with_metrics: from.fast_fn_with_metrics,
      ..self
    }
  }

  #[doc(hidden)]
  pub const fn fast_fn(self) -> FastFunction {
    let Some(f) = self.fast_fn else {
      panic!("Not a fast function");
    };
    f
  }

  #[doc(hidden)]
  pub const fn fast_fn_with_metrics(self) -> FastFunction {
    let Some(f) = self.fast_fn_with_metrics else {
      panic!("Not a fast function");
    };
    f
  }
}

/// Declares a block of Deno `#[op]`s. The first parameter determines the name of the
/// op declaration block, and is usually `deno_ops`. This block generates a function that
/// returns a [`Vec<OpDecl>`].
///
/// This can be either a compact form like:
///
/// ```no_compile
/// # use deno_core::*;
/// #[op]
/// fn op_xyz() {}
///
/// deno_core::ops!(deno_ops, [
///   op_xyz
/// ]);
///
/// // Use the ops:
/// deno_ops()
/// ```
///
/// ... or a parameterized form like so that allows passing a number of type parameters
/// to each `#[op]`:
///
/// ```no_compile
/// # use deno_core::*;
/// #[op]
/// fn op_xyz<P>() where P: Clone {}
///
/// deno_core::ops!(deno_ops,
///   parameters = [P: Clone],
///   ops = [
///     op_xyz<P>
///   ]
/// );
///
/// // Use the ops, with `String` as the parameter `P`:
/// deno_ops::<String>()
/// ```
#[macro_export]
macro_rules! ops {
  ($name:ident, parameters = [ $( $param:ident : $type:ident ),+ ], ops = [ $( $(#[$m:meta])* $( $op:ident )::+ $( < $op_param:ident > )?  ),+ $(,)? ]) => {
    pub(crate) fn $name < $( $param : $type + 'static ),+ > () -> Vec<$crate::OpDecl> {
      vec![
      $(
        $( #[ $m ] )*
        $( $op )::+ :: decl $( :: <$op_param> )? () ,
      )+
      ]
    }
  };
  ($name:ident, [ $( $(#[$m:meta])* $( $op:ident )::+ ),+ $(,)? ] ) => {
    pub(crate) fn $name() -> Vec<$crate::OpDecl> {
      use $crate::Op;
      vec![
        $( $( #[ $m ] )* $( $op )::+ :: DECL, )+
      ]
    }
  }
}

/// Return the first argument if not empty, otherwise the second.
#[macro_export]
macro_rules! or {
  ($e:expr, $fallback:expr) => {
    $e
  };
  (, $fallback:expr) => {
    $fallback
  };
}

/// Defines a Deno extension. The first parameter is the name of the extension symbol namespace to create. This is the symbol you
/// will use to refer to the extension.
///
/// Most extensions will define a combination of ops and ESM files, like so:
///
/// ```no_compile
/// #[op]
/// fn op_xyz() {
/// }
///
/// deno_core::extension!(
///   my_extension,
///   ops = [ op_xyz ],
///   esm = [ "my_script.js" ],
///   docs = "A small sample extension"
/// );
/// ```
///
/// The following options are available for the [`extension`] macro:
///
///  * deps: a comma-separated list of module dependencies, eg: `deps = [ my_other_extension ]`
///  * parameters: a comma-separated list of parameters and base traits, eg: `parameters = [ P: MyTrait ]`
///  * bounds: a comma-separated list of additional type bounds, eg: `bounds = [ P::MyAssociatedType: MyTrait ]`
///  * ops: a comma-separated list of [`OpDecl`]s to provide, eg: `ops = [ op_foo, op_bar ]`
///  * esm: a comma-separated list of ESM module filenames (see [`include_js_files`]), eg: `esm = [ dir "dir", "my_file.js" ]`
///  * js: a comma-separated list of JS filenames (see [`include_js_files`]), eg: `js = [ dir "dir", "my_file.js" ]`
///  * config: a structure-like definition for configuration parameters which will be required when initializing this extension, eg: `config = { my_param: Option<usize> }`
///  * middleware: an [`OpDecl`] middleware function with the signature `fn (OpDecl) -> OpDecl`
///  * state: a state initialization function, with the signature `fn (&mut OpState, ...) -> ()`, where `...` are parameters matching the fields of the config struct
///  * event_loop_middleware: an event-loop middleware function (see [`ExtensionBuilder::event_loop_middleware`])
///  * global_template_middleware: a global template middleware function (see [`ExtensionBuilder::global_template_middleware`])
///  * global_object_middleware: a global object middleware function (see [`ExtensionBuilder::global_object_middleware`])
///  * docs: comma separated list of toplevel #[doc=...] tags to be applied to the extension's resulting struct
#[macro_export]
macro_rules! extension {
  (
    $name:ident
    $(, deps = [ $( $dep:ident ),* ] )?
    $(, parameters = [ $( $param:ident : $type:ident ),+ ] )?
    $(, bounds = [ $( $bound:path : $bound_type:ident ),+ ] )?
    $(, ops_fn = $ops_symbol:ident $( < $ops_param:ident > )? )?
    $(, ops = [ $( $(#[$m:meta])* $( $op:ident )::+ $( < $( $op_param:ident ),* > )?  ),+ $(,)? ] )?
    $(, esm_entry_point = $esm_entry_point:expr )?
    $(, esm = [ $( dir $dir_esm:expr , )? $( $esm:literal $( with_specifier $esm_specifier:expr )? ),* $(,)? ] )?
    $(, lazy_loaded_esm = [ $( dir $dir_lazy_loaded_esm:expr , )? $( $lazy_loaded_esm:literal $( with_specifier $lazy_loaded_esm_specifier:expr )? ),* $(,)? ] )?
    $(, js = [ $( dir $dir_js:expr , )? $( $js:literal ),* $(,)? ] )?
    $(, options = { $( $options_id:ident : $options_type:ty ),* $(,)? } )?
    $(, middleware = $middleware_fn:expr )?
    $(, state = $state_fn:expr )?
    $(, event_loop_middleware = $event_loop_middleware_fn:expr )?
    $(, global_template_middleware = $global_template_middleware_fn:expr )?
    $(, global_object_middleware = $global_object_middleware_fn:expr )?
    $(, external_references = [ $( $external_reference:expr ),* $(,)? ] )?
    $(, customizer = $customizer_fn:expr )?
    $(, docs = $($docblocks:expr),+)?
    $(,)?
  ) => {
    $( $(#[doc = $docblocks])+ )?
    ///
    /// An extension for use with the Deno JS runtime.
    /// To use it, provide it as an argument when instantiating your runtime:
    ///
    /// ```rust,ignore
    /// use deno_core::{ JsRuntime, RuntimeOptions };
    ///
    #[doc = concat!("let mut extensions = vec![", stringify!($name), "::init_ops_and_esm()];")]
    /// let mut js_runtime = JsRuntime::new(RuntimeOptions {
    ///   extensions,
    ///   ..Default::default()
    /// });
    /// ```
    ///
    #[allow(non_camel_case_types)]
    pub struct $name {
    }

    impl $name {
      fn ext $( <  $( $param : $type + 'static ),+ > )?() -> $crate::Extension {
        #[allow(unused_imports)]
        use $crate::Op;
        $crate::Extension {
          // Computed at compile-time, may be modified at runtime with `Cow`:
          name: stringify!($name),
          deps: &[ $( $( stringify!($dep) ),* )? ],
          // Use intermediary `const`s here to disable user expressions which
          // can't be evaluated at compile-time.
          js_files: {
            const V: std::borrow::Cow<'static, [$crate::ExtensionFileSource]> = std::borrow::Cow::Borrowed(&$crate::or!($($crate::include_js_files!( $name $( dir $dir_js , )? $( $js , )* ))?, []));
            V
          },
          esm_files: {
            const V: std::borrow::Cow<'static, [$crate::ExtensionFileSource]> = std::borrow::Cow::Borrowed(&$crate::or!($($crate::include_js_files!( $name $( dir $dir_esm , )? $( $esm $( with_specifier $esm_specifier )? , )* ))?, []));
            V
          },
          lazy_loaded_esm_files: {
            const V: std::borrow::Cow<'static, [$crate::ExtensionFileSource]> = std::borrow::Cow::Borrowed(&$crate::or!($($crate::include_lazy_loaded_js_files!( $name $( dir $dir_lazy_loaded_esm , )? $( $lazy_loaded_esm $( with_specifier $lazy_loaded_esm_specifier )? , )* ))?, []));
            V
          },
          esm_entry_point: {
            const V: Option<&'static str> = $crate::or!($(Some($esm_entry_point))?, None);
            V
          },
          ops: std::borrow::Cow::Borrowed(&[$($(
            $( #[ $m ] )*
            $( $op )::+ $( :: < $($op_param),* > )? :: DECL
          ),+)?]),
          external_references: std::borrow::Cow::Borrowed(&[ $( $external_reference ),* ]),
          event_loop_middleware: None,
          global_template_middleware: None,
          global_object_middleware: None,
          // Computed at runtime:
          op_state_fn: None,
          middleware_fn: None,
          enabled: true,
        }
      }

      // If ops were specified, add those ops to the extension.
      #[inline(always)]
      #[allow(unused_variables)]
      fn with_ops_fn $( <  $( $param : $type + 'static ),+ > )?(ext: &mut $crate::Extension)
      $( where $( $bound : $bound_type ),+ )?
      {
        // Use the ops_fn, if provided
        $crate::extension!(! __ops__ ext $( $ops_symbol $( < $ops_param > )? )? __eot__);
      }

      // Includes the state and middleware functions, if defined.
      #[inline(always)]
      #[allow(unused_variables)]
      fn with_state_and_middleware$( <  $( $param : $type + 'static ),+ > )?(ext: &mut $crate::Extension, $( $( $options_id : $options_type ),* )? )
      $( where $( $bound : $bound_type ),+ )?
      {
        $crate::extension!(! __config__ ext $( parameters = [ $( $param : $type ),* ] )? $( config = { $( $options_id : $options_type ),* } )? $( state_fn = $state_fn )? );

        $(
          ext.event_loop_middleware = Some($event_loop_middleware_fn);
        )?

        $(
          ext.global_template_middleware = Some($global_template_middleware_fn);
        )?

        $(
          ext.global_object_middleware = Some($global_object_middleware_fn);
        )?

        $(
          ext.middleware_fn = Some(Box::new($middleware_fn));
        )?
      }

      #[inline(always)]
      #[allow(unused_variables)]
      #[allow(clippy::redundant_closure_call)]
      fn with_customizer(ext: &mut $crate::Extension) {
        $( ($customizer_fn)(ext); )?
      }

      #[allow(dead_code)]
      /// Legacy function for extension instantiation.
      /// Please use `init_ops_and_esm` or `init_ops` instead
      ///
      /// # Returns
      /// an Extension object that can be used during instantiation of a JsRuntime
      #[deprecated(since="0.216.0", note="please use `init_ops_and_esm` or `init_ops` instead")]
      pub fn init_js_only $( <  $( $param : $type + 'static ),* > )? () -> $crate::Extension
      $( where $( $bound : $bound_type ),+ )?
      {
        let mut ext = Self::ext $( ::< $( $param ),+ > )?();
        Self::with_ops_fn $( ::< $( $param ),+ > )?(&mut ext);
        Self::with_customizer(&mut ext);
        ext
      }

      #[allow(dead_code)]
      /// Initialize this extension for runtime or snapshot creation. Use this
      /// function if the runtime or snapshot is not created from a (separate)
      /// snapshot, or that snapshot does not contain this extension. Otherwise
      /// use `init_ops()` instead.
      ///
      /// # Returns
      /// an Extension object that can be used during instantiation of a JsRuntime
      pub fn init_ops_and_esm $( <  $( $param : $type + 'static ),+ > )? ( $( $( $options_id : $options_type ),* )? ) -> $crate::Extension
      $( where $( $bound : $bound_type ),+ )?
      {
        let mut ext = Self::ext $( ::< $( $param ),+ > )?();
        Self::with_ops_fn $( ::< $( $param ),+ > )?(&mut ext);
        Self::with_state_and_middleware $( ::< $( $param ),+ > )?(&mut ext, $( $( $options_id , )* )? );
        Self::with_customizer(&mut ext);
        ext
      }

      #[allow(dead_code)]
      /// Initialize this extension for runtime or snapshot creation, excluding
      /// its JavaScript sources and evaluation. This is used when the runtime
      /// or snapshot is created from a (separate) snapshot which includes this
      /// extension in order to avoid evaluating the JavaScript twice.
      ///
      /// # Returns
      /// an Extension object that can be used during instantiation of a JsRuntime
      pub fn init_ops $( <  $( $param : $type + 'static ),+ > )? ( $( $( $options_id : $options_type ),* )? ) -> $crate::Extension
      $( where $( $bound : $bound_type ),+ )?
      {
        let mut ext = Self::ext $( ::< $( $param ),+ > )?();
        Self::with_ops_fn $( ::< $( $param ),+ > )?(&mut ext);
        Self::with_state_and_middleware $( ::< $( $param ),+ > )?(&mut ext, $( $( $options_id , )* )? );
        Self::with_customizer(&mut ext);
        ext.js_files = std::borrow::Cow::Borrowed(&[]);
        ext.esm_files = std::borrow::Cow::Borrowed(&[]);
        ext.esm_entry_point = None;
        ext
      }
    }
  };

  // This branch of the macro generates a config object that calls the state function with itself.
  (! __config__ $ext:ident $( parameters = [ $( $param:ident : $type:ident ),+ ] )? config = { $( $options_id:ident : $options_type:ty ),* } $( state_fn = $state_fn:expr )? ) => {
    {
      #[doc(hidden)]
      struct Config $( <  $( $param : $type + 'static ),+ > )? {
        $( pub $options_id : $options_type , )*
        $( __phantom_data: ::std::marker::PhantomData<($( $param ),+)>, )?
      }
      let config = Config {
        $( $options_id , )*
        $( __phantom_data: ::std::marker::PhantomData::<($( $param ),+)>::default() )?
      };

      let state_fn: fn(&mut $crate::OpState, Config $( <  $( $param ),+ > )? ) = $(  $state_fn  )?;
      $ext.op_state_fn = Some(Box::new(move |state: &mut $crate::OpState| {
        state_fn(state, config);
      }));
    }
  };

  (! __config__ $ext:ident $( parameters = [ $( $param:ident : $type:ident ),+ ] )? $( state_fn = $state_fn:expr )? ) => {
    $( $ext.op_state_fn = Some(Box::new($state_fn)); )?
  };

  (! __ops__ $ext:ident __eot__) => {
  };

  (! __ops__ $ext:ident $ops_symbol:ident __eot__) => {
    $ext.ops.to_mut().extend($ops_symbol())
  };

  (! __ops__ $ext:ident $ops_symbol:ident < $ops_param:ident > __eot__) => {
    $ext.ops.to_mut().extend($ops_symbol::<$ops_param>())
  };
}

pub struct Extension {
  pub name: &'static str,
  pub deps: &'static [&'static str],
  pub js_files: Cow<'static, [ExtensionFileSource]>,
  pub esm_files: Cow<'static, [ExtensionFileSource]>,
  pub lazy_loaded_esm_files: Cow<'static, [ExtensionFileSource]>,
  pub esm_entry_point: Option<&'static str>,
  pub ops: Cow<'static, [OpDecl]>,
  pub external_references: Cow<'static, [v8::ExternalReference<'static>]>,
  pub event_loop_middleware: Option<EventLoopMiddlewareFn>,
  pub global_template_middleware: Option<GlobalTemplateMiddlewareFn>,
  pub global_object_middleware: Option<GlobalObjectMiddlewareFn>,
  pub op_state_fn: Option<Box<OpStateFn>>,
  pub middleware_fn: Option<Box<OpMiddlewareFn>>,
  pub enabled: bool,
}

impl Default for Extension {
  fn default() -> Self {
    Self {
      name: "DEFAULT",
      deps: &[],
      js_files: Cow::Borrowed(&[]),
      esm_files: Cow::Borrowed(&[]),
      lazy_loaded_esm_files: Cow::Borrowed(&[]),
      esm_entry_point: None,
      ops: Cow::Borrowed(&[]),
      external_references: Cow::Borrowed(&[]),
      event_loop_middleware: None,
      global_template_middleware: None,
      global_object_middleware: None,
      op_state_fn: None,
      middleware_fn: None,
      enabled: true,
    }
  }
}

// Note: this used to be a trait, but we "downgraded" it to a single concrete type
// for the initial iteration, it will likely become a trait in the future
impl Extension {
  #[deprecated(note = "Use Extension { ..., ..Default::default() }")]
  #[allow(deprecated)]
  pub fn builder(name: &'static str) -> ExtensionBuilder {
    ExtensionBuilder {
      name,
      ..Default::default()
    }
  }

  #[deprecated(note = "Use Extension { ..., ..Default::default() }")]
  #[allow(deprecated)]
  pub fn builder_with_deps(
    name: &'static str,
    deps: &'static [&'static str],
  ) -> ExtensionBuilder {
    ExtensionBuilder {
      name,
      deps,
      ..Default::default()
    }
  }

  /// Check if dependencies have been loaded, and errors if either:
  /// - The extension is depending on itself or an extension with the same name.
  /// - A dependency hasn't been loaded yet.
  pub fn check_dependencies(&self, previous_exts: &[Extension]) {
    'dep_loop: for dep in self.deps {
      if dep == &self.name {
        panic!("Extension '{}' is either depending on itself or there is another extension with the same name", self.name);
      }

      for ext in previous_exts {
        if dep == &ext.name {
          continue 'dep_loop;
        }
      }

      panic!("Extension '{}' is missing dependency '{dep}'", self.name);
    }
  }

  /// returns JS source code to be loaded into the isolate (either at snapshotting,
  /// or at startup).  as a vector of a tuple of the file name, and the source code.
  pub fn get_js_sources(&self) -> &[ExtensionFileSource] {
    self.js_files.as_ref()
  }

  pub fn get_esm_sources(&self) -> &[ExtensionFileSource] {
    self.esm_files.as_ref()
  }

  pub fn get_lazy_loaded_esm_sources(&self) -> &[ExtensionFileSource] {
    self.lazy_loaded_esm_files.as_ref()
  }

  pub fn get_esm_entry_point(&self) -> Option<&'static str> {
    self.esm_entry_point
  }

  /// Called at JsRuntime startup to initialize ops in the isolate.
  pub fn init_ops(&mut self) -> &[OpDecl] {
    if !self.enabled {
      for op in self.ops.to_mut() {
        op.enabled = false;
      }
    }
    self.ops.as_ref()
  }

  /// Allows setting up the initial op-state of an isolate at startup.
  pub fn take_state(&mut self, state: &mut OpState) {
    if let Some(op_fn) = self.op_state_fn.take() {
      op_fn(state);
    }
  }

  /// Middleware should be called before init_ops
  pub fn take_middleware(&mut self) -> Option<Box<OpMiddlewareFn>> {
    self.middleware_fn.take()
  }

  pub fn get_event_loop_middleware(&mut self) -> Option<EventLoopMiddlewareFn> {
    self.event_loop_middleware
  }

  pub fn get_global_template_middleware(
    &mut self,
  ) -> Option<GlobalTemplateMiddlewareFn> {
    self.global_template_middleware
  }

  pub fn get_global_object_middleware(
    &mut self,
  ) -> Option<GlobalObjectMiddlewareFn> {
    self.global_object_middleware
  }

  pub fn get_external_references(
    &mut self,
  ) -> &[v8::ExternalReference<'static>] {
    self.external_references.as_ref()
  }

  pub fn enabled(self, enabled: bool) -> Self {
    Self { enabled, ..self }
  }

  pub fn disable(self) -> Self {
    self.enabled(false)
  }
}

// Provides a convenient builder pattern to declare Extensions
#[derive(Default)]
pub struct ExtensionBuilder {
  js: Vec<ExtensionFileSource>,
  esm: Vec<ExtensionFileSource>,
  lazy_loaded_esm: Vec<ExtensionFileSource>,
  esm_entry_point: Option<&'static str>,
  ops: Vec<OpDecl>,
  state: Option<Box<OpStateFn>>,
  middleware: Option<Box<OpMiddlewareFn>>,
  event_loop_middleware: Option<EventLoopMiddlewareFn>,
  global_template_middleware: Option<GlobalTemplateMiddlewareFn>,
  global_object_middleware: Option<GlobalObjectMiddlewareFn>,
  external_references: Vec<ExternalReference<'static>>,
  name: &'static str,
  deps: &'static [&'static str],
}

impl ExtensionBuilder {
  pub fn js(&mut self, js_files: Vec<ExtensionFileSource>) -> &mut Self {
    self.js.extend(js_files);
    self
  }

  pub fn esm(&mut self, esm_files: Vec<ExtensionFileSource>) -> &mut Self {
    self.esm.extend(esm_files);
    self
  }

  pub fn lazy_loaded_esm(
    &mut self,
    lazy_loaded_esm_files: Vec<ExtensionFileSource>,
  ) -> &mut Self {
    self.lazy_loaded_esm.extend(lazy_loaded_esm_files);
    self
  }

  pub fn esm_entry_point(&mut self, entry_point: &'static str) -> &mut Self {
    self.esm_entry_point = Some(entry_point);
    self
  }

  pub fn ops(&mut self, ops: Vec<OpDecl>) -> &mut Self {
    self.ops.extend(ops);
    self
  }

  pub fn state<F>(&mut self, op_state_fn: F) -> &mut Self
  where
    F: FnOnce(&mut OpState) + 'static,
  {
    self.state = Some(Box::new(op_state_fn));
    self
  }

  pub fn middleware<F>(&mut self, middleware_fn: F) -> &mut Self
  where
    F: Fn(OpDecl) -> OpDecl + 'static,
  {
    self.middleware = Some(Box::new(middleware_fn));
    self
  }

  pub fn event_loop_middleware<F>(
    &mut self,
    middleware_fn: EventLoopMiddlewareFn,
  ) -> &mut Self {
    self.event_loop_middleware = Some(middleware_fn);
    self
  }

  pub fn global_template_middleware<F>(
    &mut self,
    middleware_fn: GlobalTemplateMiddlewareFn,
  ) -> &mut Self {
    self.global_template_middleware = Some(middleware_fn);
    self
  }

  pub fn global_object_middleware<F>(
    &mut self,
    middleware_fn: GlobalObjectMiddlewareFn,
  ) -> &mut Self {
    self.global_object_middleware = Some(middleware_fn);
    self
  }

  pub fn external_references(
    &mut self,
    external_references: Vec<ExternalReference<'static>>,
  ) -> &mut Self {
    self.external_references.extend(external_references);
    self
  }

  /// Consume the [`ExtensionBuilder`] and return an [`Extension`].
  pub fn take(self) -> Extension {
    Extension {
      name: self.name,
      deps: self.deps,
      js_files: Cow::Owned(self.js),
      esm_files: Cow::Owned(self.esm),
      lazy_loaded_esm_files: Cow::Owned(self.lazy_loaded_esm),
      esm_entry_point: self.esm_entry_point,
      ops: Cow::Owned(self.ops),
      external_references: Cow::Owned(self.external_references),
      event_loop_middleware: self.event_loop_middleware,
      global_template_middleware: self.global_template_middleware,
      global_object_middleware: self.global_object_middleware,
      op_state_fn: self.state,
      middleware_fn: self.middleware,
      enabled: true,
    }
  }

  pub fn build(&mut self) -> Extension {
    Extension {
      name: self.name,
      deps: std::mem::take(&mut self.deps),
      js_files: Cow::Owned(std::mem::take(&mut self.js)),
      esm_files: Cow::Owned(std::mem::take(&mut self.esm)),
      lazy_loaded_esm_files: Cow::Owned(std::mem::take(
        &mut self.lazy_loaded_esm,
      )),
      esm_entry_point: self.esm_entry_point.take(),
      ops: Cow::Owned(std::mem::take(&mut self.ops)),
      external_references: Cow::Owned(std::mem::take(
        &mut self.external_references,
      )),
      event_loop_middleware: self.event_loop_middleware.take(),
      global_template_middleware: self.global_template_middleware.take(),
      global_object_middleware: self.global_object_middleware.take(),
      op_state_fn: self.state.take(),
      middleware_fn: self.middleware.take(),
      enabled: true,
    }
  }
}

/// Helps embed JS files in an extension. Returns a vector of
/// `ExtensionFileSource`, that represent the filename and source code. All
/// specified files are rewritten into "ext:<extension_name>/<file_name>".
///
/// An optional "dir" option can be specified to prefix all files with a
/// directory name.
///
/// ```ignore
/// /// Example (for "my_extension"):
/// include_js_files!(
///   "01_hello.js",
///   "02_goodbye.js",
/// )
/// // Produces following specifiers:
/// // - "ext:my_extension/01_hello.js"
/// // - "ext:my_extension/02_goodbye.js"
///
/// /// Example with "dir" option (for "my_extension"):
/// include_js_files!(
///   dir "js",
///   "01_hello.js",
///   "02_goodbye.js",
/// )
/// // Produces following specifiers:
/// - "ext:my_extension/js/01_hello.js"
/// - "ext:my_extension/js/02_goodbye.js"
/// ```
#[cfg(not(feature = "include_js_files_for_snapshotting"))]
#[macro_export]
macro_rules! include_js_files {
  ($name:ident dir $dir:expr, $($file:literal $(with_specifier $esm_specifier:expr)?,)+) => {
    [
      $($crate::ExtensionFileSource {
        specifier: $crate::or!($($esm_specifier)?, concat!("ext:", stringify!($name), "/", $file)),
        code: $crate::ExtensionFileSourceCode::IncludedInBinary(
          include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $dir, "/", $file))
        ),
      },)+
    ]
  };

  ($name:ident $($file:literal $(with_specifier $esm_specifier:expr)?,)+) => {
    [
      $($crate::ExtensionFileSource {
        specifier: $crate::or!($($esm_specifier)?, concat!("ext:", stringify!($name), "/", $file)),
        code: $crate::ExtensionFileSourceCode::IncludedInBinary(
          include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $file))
        ),
      },)+
    ]
  };
}

#[cfg(feature = "include_js_files_for_snapshotting")]
#[macro_export]
macro_rules! include_js_files {
  ($name:ident dir $dir:expr, $($file:literal $(with_specifier $esm_specifier:expr)?,)+) => {
    [
      $($crate::ExtensionFileSource {
        specifier: $crate::or!($($esm_specifier)?, concat!("ext:", stringify!($name), "/", $file)),
        code: $crate::ExtensionFileSourceCode::LoadedFromFsDuringSnapshot(
          concat!(env!("CARGO_MANIFEST_DIR"), "/", $dir, "/", $file)
        ),
      },)+
    ]
  };

  ($name:ident $($file:literal $(with_specifier $esm_specifier:expr)?,)+) => {
    [
      $($crate::ExtensionFileSource {
        specifier: $crate::or!($($esm_specifier)?, concat!("ext:", stringify!($name), "/", $file)),
        code: $crate::ExtensionFileSourceCode::LoadedFromFsDuringSnapshot(
          concat!(env!("CARGO_MANIFEST_DIR"), "/", $file)
        ),
      },)+
    ]
  };
}

#[macro_export]
macro_rules! include_lazy_loaded_js_files {
  ($name:ident dir $dir:expr, $($file:literal $(with_specifier $esm_specifier:expr)?,)+) => {
    [
      $($crate::ExtensionFileSource {
        specifier: $crate::or!($($esm_specifier)?, concat!("ext:", stringify!($name), "/", $file)),
        code: $crate::ExtensionFileSourceCode::IncludedInBinary(
          include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $dir, "/", $file))
        ),
      },)+
    ]
  };

  ($name:ident $($file:literal $(with_specifier $esm_specifier:expr)?,)+) => {
    [
      $($crate::ExtensionFileSource {
        specifier: $crate::or!($($esm_specifier)?, concat!("ext:", stringify!($name), "/", $file)),
        code: $crate::ExtensionFileSourceCode::IncludedInBinary(
          include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $file))
        ),
      },)+
    ]
  };
}
