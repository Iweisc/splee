// Zips what the browser loads into dist/splee-extension.zip, for stores and for installing without the repo.
// A zip writer this small saves a dependency: stored names, deflated data and the central directory.
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { deflateRawSync, crc32 } from "node:zlib";

const root = new URL("..", import.meta.url);
const icons = readdirSync(new URL("icons", root)).map((f) => `icons/${f}`);
const files = ["manifest.json", "popup.html", "popup.css", "popup.js", "selectors.js", "readers.js", ...icons];

// 1 January 1980, so the same files always make the same zip.
const DOS_DATE = (1 << 5) | 1;
const local = [];
const central = [];
let offset = 0;
for (const name of files) {
  const data = readFileSync(new URL(name, root));
  const packed = deflateRawSync(data);
  const nameBytes = Buffer.from(name);
  const header = Buffer.alloc(30);
  header.writeUInt32LE(0x04034b50, 0);
  header.writeUInt16LE(20, 4);
  header.writeUInt16LE(8, 8);
  header.writeUInt16LE(DOS_DATE, 12);
  header.writeUInt32LE(crc32(data), 14);
  header.writeUInt32LE(packed.length, 18);
  header.writeUInt32LE(data.length, 22);
  header.writeUInt16LE(nameBytes.length, 26);
  const entry = Buffer.alloc(46);
  entry.writeUInt32LE(0x02014b50, 0);
  entry.writeUInt16LE(20, 4);
  entry.writeUInt16LE(20, 6);
  entry.writeUInt16LE(8, 10);
  entry.writeUInt16LE(DOS_DATE, 14);
  entry.writeUInt32LE(crc32(data), 16);
  entry.writeUInt32LE(packed.length, 20);
  entry.writeUInt32LE(data.length, 24);
  entry.writeUInt16LE(nameBytes.length, 28);
  entry.writeUInt32LE(offset, 42);
  local.push(header, nameBytes, packed);
  central.push(entry, nameBytes);
  offset += header.length + nameBytes.length + packed.length;
}
const directory = Buffer.concat(central);
const end = Buffer.alloc(22);
end.writeUInt32LE(0x06054b50, 0);
end.writeUInt16LE(files.length, 8);
end.writeUInt16LE(files.length, 10);
end.writeUInt32LE(directory.length, 12);
end.writeUInt32LE(offset, 16);

mkdirSync(new URL("dist", root), { recursive: true });
const out = new URL("dist/splee-extension.zip", root);
writeFileSync(out, Buffer.concat([...local, directory, end]));
console.log(`Wrote ${out.pathname}`);
