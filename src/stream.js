// Packet stream of one ErsatzTV script session. ErsatzTV reads one frame per
// output frame; the stream is paced by the wall clock with a lead, so it
// follows `-readrate 1.0` with its initial burst but never runs far ahead of
// the live VizRS graphics.

import { framePacket, repeatPacket } from "./packets.js";

const TICK_MS = 20;

export function streamFrames(capture, response, { fps, leadSeconds }) {
  const frameMs = 1000 / fps;
  const leadFrames = Math.round(leadSeconds * fps);
  const started = performance.now();
  let sent = 0;
  let sentSeq = -1;
  let closed = false;
  let waiting = false;

  const write = (packet) => {
    if (!response.write(packet)) {
      waiting = true;
      response.once("drain", () => {
        waiting = false;
        pump();
      });
    }
  };

  function pump() {
    if (closed || waiting) return;

    const due = Math.floor((performance.now() - started) / frameMs) + 1 + leadFrames;

    while (sent < due && !waiting) {
      const frame = capture.frame;

      if (frame.seq !== sentSeq) {
        sentSeq = frame.seq;
        sent += 1;
        write(framePacket(frame));
      } else {
        const frames = due - sent;
        sent += frames;
        write(repeatPacket(frames));
      }
    }
  }

  const timer = setInterval(pump, TICK_MS);
  const stop = () => {
    closed = true;
    clearInterval(timer);
  };

  response.on("close", stop);
  response.on("error", stop);
  pump();

  return { stop, get sent() { return sent; } };
}
