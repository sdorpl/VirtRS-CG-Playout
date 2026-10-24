// Frame stream of one session: ETV packets for an ErsatzTV script element, or
// raw yuva420p frames for ffmpeg (the ffmpeg overlay). The reader takes one
// frame per output frame; the stream is paced by the wall clock with a lead,
// so it follows `-readrate 1.0` with its initial burst but never runs far
// ahead of the live VizRS graphics. Every output frame takes the picture that
// was current at its own moment (as a playout blending the current graphics
// into each frame), so late writes do not turn motion into repeats and jumps.

import { framePacket, repeatPacket } from "./packets.js";
import { framePicture } from "./yuva.js";

const TICK_MS = 20;
// Pictures are dated when Chromium drew them and arrive decoded up to ~100 ms
// later; output frames look this far back so each finds the picture of its moment.
export const SAMPLE_DELAY_MS = 100;

/** ETV packets: a picture once, then one `Repeat` for any run of the same picture. */
export const packetFormat = { picture: framePacket, repeat: (frames) => repeatPacket(frames), run: Infinity };

/** Raw yuva420p: every output frame carries the whole picture. */
export function yuvaFormat(width, height) {
  const picture = (frame) => framePicture(frame, width, height);
  return { picture, repeat: (frames, frame) => picture(frame), run: 1 };
}

/**
 * `paused`: write nothing until `resume()`. A resumed stream starts its
 * timeline (and lead) again from the frames already sent.
 */
export function streamFrames(capture, response, { fps, leadSeconds, format = packetFormat, paused = false }) {
  const frameMs = 1000 / fps;
  const leadFrames = Math.round(leadSeconds * fps);
  let started = performance.now();
  // Moment of output frame `n`: the first `leadFrames` go out at once.
  const frameAt = (n) => {
    const time = started + Math.max(0, n - leadFrames) * frameMs - SAMPLE_DELAY_MS;
    return capture.frameAt?.(time) ?? capture.frame;
  };
  let sent = 0;
  let sentSeq = -1;
  let closed = false;
  let waiting = false;
  let timer = null;

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
    if (closed || waiting || paused) return;

    const due = Math.floor((performance.now() - started) / frameMs) + 1 + leadFrames;

    while (sent < due && !waiting) {
      const frame = frameAt(sent);

      if (frame.seq !== sentSeq) {
        sentSeq = frame.seq;
        sent += 1;
        write(format.picture(frame));
      } else {
        let frames = 1;
        while (frames < format.run && sent + frames < due && frameAt(sent + frames).seq === sentSeq) frames += 1;
        sent += frames;
        write(format.repeat(frames, frame));
      }
    }
  }

  const run = () => {
    clearInterval(timer);
    timer = setInterval(pump, TICK_MS);
    pump();
  };
  const stop = () => {
    closed = true;
    clearInterval(timer);
  };

  response.on("close", stop);
  response.on("error", stop);
  if (!paused) run();

  return {
    stop,
    pause() {
      paused = true;
      clearInterval(timer);
    },
    resume() {
      if (!paused || closed) return;
      paused = false;
      started = performance.now() - Math.max(0, sent - leadFrames) * frameMs;
      run();
    },
    get sent() { return sent; },
  };
}
