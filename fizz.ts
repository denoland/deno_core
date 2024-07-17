interface Foobar {
  a: string;
  b: number;
  c: boolean;
}

console.log("hello there");

function boom(): never {
  throw new Error("boom" as string);
}

boom();
