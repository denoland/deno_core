
const hexSliceLookupTable = (function () {
  const alphabet = "0123456789abcdef";
  const table = new Array(256);
  for (let i = 0; i < 16; ++i) {
    const i16 = i * 16;
    for (let j = 0; j < 16; ++j) {
      table[i16 + j] = alphabet[i] + alphabet[j];
    }
  }
  return table;
})();

function generateId(size: number) {
  let out = "";
  for (let i = 0; i < size / 4; i += 1) {
    const r32 = (Math.random() * 2 ** 32) >>> 0;
    out += hexSliceLookupTable[(r32 >> 24) & 0xff];
    out += hexSliceLookupTable[(r32 >> 16) & 0xff];
    out += hexSliceLookupTable[(r32 >> 8) & 0xff];
    out += hexSliceLookupTable[r32 & 0xff];
  }
  return out;
}


const start = Date.now();
while (start + 1000 > Date.now()) {
  const id = generateId(16);
  op_fast(id);
}
