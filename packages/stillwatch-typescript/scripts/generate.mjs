import { readFile, writeFile } from "node:fs/promises";
import { compile } from "json-schema-to-typescript";

const names = [
  "AlertFeed", "AlertReceipt", "AnalysisStatus", "Bootstrap", "Camera", "CameraConfig", "CameraPage",
  "CameraPatch", "CaptureDevices", "Change", "CoreStatus", "DeviceCredentials", "Devices",
  "EvidencePacket", "Health", "Incident", "IncidentPage", "OnvifDevices", "OnvifProfiles",
  "OnvifProfilesRequest", "OnvifStream", "OnvifStreamRequest", "PairingOffer",
  "PairingRequest", "Problem", "ReplayUpload", "SemanticStatus", "Snapshot", "SourceProbe", "SourceProbeRequest",
  "StreamGrant", "StreamRequest",
];
for (const name of names) {
  const source = new URL("../../contracts/schemas/" + name + ".json", import.meta.url);
  const destination = new URL("../src/generated/" + name + ".ts", import.meta.url);
  const schema = JSON.parse(await readFile(source, "utf8"));
  // The generator expects draft-07 definitions. Schemars emits draft 2020-12 $defs.
  if (schema.$defs) {
    schema.definitions = schema.$defs;
    delete schema.$defs;
  }
  const compatible = JSON.parse(JSON.stringify(schema).replaceAll("#/$defs/", "#/definitions/"));
  const result = await compile(compatible, name, {
    bannerComment: "// Generated from sw-protocol schemas. Regenerate with npm run generate.",
    unreachableDefinitions: false,
  });
  if (process.argv.includes("--check")) {
    if (await readFile(destination, "utf8") !== result) throw Error("Schema drift in " + name);
  } else {
    await writeFile(destination, result);
  }
}
