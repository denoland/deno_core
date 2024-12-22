const scenario: "js" | "objwrap" | "clean" = Deno.args[0];
if (
  !scenario ||
  (scenario !== "js" && scenario !== "objwrap" && scenario !== "clean")
) {
  console.error(
    'Please provide scenario as first arg (accepted values are "js", "objwrap", "clean"',
  );
  Deno.exit(1);
}
const count = parseInt(Deno.args[1] || "1000");

function jsTemplate(n: number) {
  return `
export class JsDOMPoint${n} {
  #x${n}: number;
  #y${n}: number;
  #z${n}: number;
  #w${n}: number;
  constructor(x${n} = 0, y${n} = 0, z${n} = 0, w${n} = 0) {
    this.#x${n} = x${n};
    this.#y${n} = y${n};
    this.#z${n} = z${n};
    this.#w${n} = w${n};
  }

  static fromPoint(value: {x?: number, y?: number, z?: number, w?: number}): JsDOMPoint${n} {
    return new JsDOMPoint${n}(value.x, value.y, value.z, value.w);
  }

  fromPoint(value: {x?: number, y?: number, z?: number, w?: number}): JsDOMPoint${n} {
    return new JsDOMPoint${n}(value.x, value.y, value.z, value.w);
  }

  get x${n}(): number {
    return this.#x${n};
  }
  set x${n}(value: number) {
    this.#x${n} = value;
  }
  get y${n}(): number {
    return this.#y${n};
  }
  set y${n}(value: number) {
    this.#y${n} = value;
  }
  get z${n}(): number {
    return this.#z${n};
  }
  set z${n}(value: number) {
    this.#z${n} = value;
  }
  get w${n}(): number {
    return this.#w${n};
  }
  set w${n}(value: number) {
    this.#w${n} = value;
  }

  wrappingSmi(value: number): number {
    return value;
  }
}
  `;
}

function rustTemplate(n: number) {
  return `
pub struct DOMPoint${n} {
  pub x: f64,
  pub y: f64,
  pub z: f64,
  pub w: f64,
}

impl GarbageCollected for DOMPoint${n} {}
impl DOMPoint${n} {
  fn from_point_inner(
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint${n}, AnyError> {
    fn get(
      scope: &mut v8::HandleScope,
      other: v8::Local<v8::Object>,
      key: &str,
    ) -> Option<f64> {
      let key = v8::String::new(scope, key).unwrap();
      other
        .get(scope, key.into())
        .map(|x| x.to_number(scope).unwrap().value())
    }

    Ok(DOMPoint${n} {
      x: get(scope, other, "x").unwrap_or(0.0),
      y: get(scope, other, "y").unwrap_or(0.0),
      z: get(scope, other, "z").unwrap_or(0.0),
      w: get(scope, other, "w").unwrap_or(0.0),
    })
  }
}

#[op2]
impl DOMPoint${n} {
  #[constructor]
  #[cppgc]
  fn new(
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    w: Option<f64>,
  ) -> DOMPoint${n} {
    DOMPoint${n} {
      x: x.unwrap_or(0.0),
      y: y.unwrap_or(0.0),
      z: z.unwrap_or(0.0),
      w: w.unwrap_or(0.0),
    }
  }

  #[required(1)]
  #[static_method]
  #[cppgc]
  fn from_point(
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint${n}, AnyError> {
    DOMPoint${n}::from_point_inner(scope, other)
  }

  #[required(1)]
  #[cppgc]
  fn from_point(
    &self,
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint${n}, AnyError> {
    DOMPoint${n}::from_point_inner(scope, other)
  }

  #[fast]
  #[getter]
  fn x${n}(&self) -> f64 {
    self.x
  }

  #[fast]
  #[setter]
  fn x${n}(&self, _: f64) {}

  #[fast]
  #[getter]
  fn y${n}(&self) -> f64 {
    self.y
  }
  #[fast]
  #[setter]
  fn y${n}(&self, _: f64){}

  #[fast]
  #[getter]
  fn w${n}(&self) -> f64 {
    self.w
  }
  #[fast]
  #[setter]
  fn w${n}(&self, _: f64){}

  #[fast]
  #[getter]
  fn z${n}(&self) -> f64 {
    self.z
  }
  #[fast]
  #[setter]
  fn z${n}(&self, _: f64){}

  #[fast]
  fn wrapping_smi(&self, #[smi] t: u32) -> u32 {
    t
  }
}
  `;
}

const filesToUpdate = [
  "testing/checkin/runner/extensions.rs",
  "testing/checkin/runner/ops.rs",
  "testing/checkin/runtime/object.ts",
];

function insertToFile(
  filename: string,
  scenario: "js" | "objwrap" | "clean",
  count: number,
  out: string[],
) {
  if (filename.endsWith("/ops.rs")) {
    if (scenario === "js") {
      return;
    }
    for (let i = 0; i < count; i++) {
      out.push(rustTemplate(i));
    }
  } else if (filename.endsWith("/object.ts")) {
    if (scenario === "objwrap") {
      const imports = Array.from(Array(count).keys()).map((i) => `DOMPoint${i}`)
        .join(", ");
      out.push(`import { ${imports} } from "ext:core/ops";`);
      out.push(`export { ${imports} };`);
    } else {
      for (let i = 0; i < count; i++) {
        out.push(jsTemplate(i));
      }
    }
  } else if (filename.endsWith("/extensions.rs")) {
    if (scenario === "js") {
      return;
    }
    for (let i = 0; i < count; i++) {
      out.push(`ops::DOMPoint${i},`);
    }
  }
}

for (const file of filesToUpdate) {
  const content = Deno.readTextFileSync(file);
  const lines = content.split("\n");
  const newLines = [];
  const name = scenario.toUpperCase();
  let remove = false;
  let alreadyPushed = false;
  let i = 0;
  while (i < lines.length) {
    const current = lines[i];
    if (
      current.includes("END INSERT HERE: ")
    ) {
      remove = false;
      alreadyPushed = true;
      newLines.push(current);
    } else if (
      current.includes("INSERT HERE: ")
    ) {
      remove = true;
      alreadyPushed = true;
      newLines.push(current);
    }
    if (current.includes("END INSERT HERE: " + name)) {
      i++;
      if (!alreadyPushed) newLines.push(current);
      alreadyPushed = false;
      continue;
    } else if (current.includes("INSERT HERE: " + name)) {
      i++;
      if (!alreadyPushed) newLines.push(current);
      alreadyPushed = false;
      insertToFile(file, scenario, count, newLines);
      continue;
    }
    if (!remove) {
      if (!alreadyPushed) newLines.push(current);
      alreadyPushed = false;
    }

    i++;
  }
  Deno.writeTextFileSync(file, newLines.join("\n"));
}
