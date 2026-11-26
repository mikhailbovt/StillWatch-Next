import { maxImages, maxImageBytes, maxRequestBytes } from "./config.js";

const metadataLimit = 12 * 1024;
const digest = /^[a-f0-9]{64}$/;
const uuid = /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/;
const counter = /^(?:0|[1-9][0-9]{0,19})$/;
const promptVersions = {
  fire: ["fire-evidence-1", "fire-evidence-2", "fire-evidence-3"],
  medical: ["medical-evidence-1"],
};

export async function decodePacket(buffer) {
  if (buffer.byteLength < 5 || buffer.byteLength > maxRequestBytes) throw new Error("invalid_packet");
  const bytes = new Uint8Array(buffer);
  const length = new DataView(buffer).getUint32(0, true);
  if (length < 2 || length > metadataLimit || length > bytes.length - 4) throw new Error("invalid_packet");
  const meta = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(4, 4 + length)));
  if (meta.schema_version !== "1.0" || ![meta.packet_id, meta.incident_id, meta.camera_id].every(id => uuid.test(id))
      || ![meta.source_epoch, meta.scene_revision, meta.evidence_version].every(value => typeof value === "string" && counter.test(value))
      || !["fire", "medical"].includes(meta.category)
      || meta.context?.local_facts?.type !== meta.category
      || !promptVersions[meta.category].includes(meta.context.prompt_version)
      || !Array.isArray(meta.attachments) || meta.attachments.length < 1 || meta.attachments.length > maxImages
      || !Number.isFinite(meta.timeout_ms) || meta.timeout_ms < 1 || meta.timeout_ms > 15_000
      || !["cloud_only", "local_only", "prefer_local"].includes(meta.route)) {
    throw new Error("invalid_packet");
  }
  let offset = 4 + length;
  let latest = -Infinity;
  const seen = new Set();
  const images = [];
  for (const image of meta.attachments) {
    if (typeof image.id !== "string" || !/^f\d{1,2}$/.test(image.id) || seen.has(image.id)
        || !digest.test(image.sha256) || !["overview", "crop"].includes(image.kind)
        || !Number.isFinite(image.source_s) || image.source_s < latest
        || !Number.isInteger(image.bytes) || image.bytes < 4 || image.bytes > maxImageBytes
        || offset + image.bytes > bytes.length) throw new Error("invalid_packet");
    const jpeg = bytes.subarray(offset, offset + image.bytes);
    const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", jpeg));
    const sha = Array.from(hash, byte => byte.toString(16).padStart(2, "0")).join("");
    if (sha !== image.sha256 || jpeg[0] !== 255 || jpeg[1] !== 216 || jpeg[2] !== 255
        || jpeg.at(-2) !== 255 || jpeg.at(-1) !== 217) throw new Error("invalid_packet");
    seen.add(image.id);
    latest = image.source_s;
    images.push({ ...image, jpeg });
    offset += image.bytes;
  }
  if (offset !== bytes.length || !images.some(image => image.kind === "overview")) throw new Error("invalid_packet");
  return { meta, images };
}
