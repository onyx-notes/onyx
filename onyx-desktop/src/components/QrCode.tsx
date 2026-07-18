// A self-contained QR code: encodes `data` and renders it as an inline SVG
// (no external assets, no innerHTML). Always black-on-white regardless of
// the app theme — QR scanners need the contrast and quiet zone.

import { createMemo } from "solid-js";
import qrcode from "qrcode-generator";

const QUIET_ZONE = 4; // modules of white margin the spec requires

export default function QrCode(props: { data: string; size?: number }) {
  const model = createMemo(() => {
    const qr = qrcode(0, "M"); // smallest fitting version, medium ECC
    qr.addData(props.data);
    qr.make();
    const count = qr.getModuleCount();
    let path = "";
    for (let row = 0; row < count; row++) {
      for (let col = 0; col < count; col++) {
        if (qr.isDark(row, col)) path += `M${col} ${row}h1v1h-1z`;
      }
    }
    return { count, path };
  });

  const px = () => props.size ?? 220;
  const extent = () => model().count + QUIET_ZONE * 2;

  return (
    <svg
      width={px()}
      height={px()}
      viewBox={`${-QUIET_ZONE} ${-QUIET_ZONE} ${extent()} ${extent()}`}
      shape-rendering="crispEdges"
      role="img"
      aria-label="QR code"
      style={{ background: "#fff", "border-radius": "8px", display: "block" }}
    >
      <path d={model().path} fill="#000" />
    </svg>
  );
}
