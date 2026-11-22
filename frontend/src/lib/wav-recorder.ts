/**
 * Records from the microphone and hands back a WAV, since Twilio plays WAV and MP3 but not the WebM or MP4 that
 * MediaRecorder makes. The browser decodes its own recording, and an OfflineAudioContext mixes it down to mono at 8 kHz,
 * the quality a phone call carries anyway.
 */

const RATE = 8000;

export type Recording = {
  /** The WAV once it stops, on stop() or by itself after `maxSecs`. Null when it was cancelled. */
  wav: Promise<Blob | null>;
  stop: () => void;
  cancel: () => void;
};

export async function startRecording(maxSecs: number): Promise<Recording> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const release = () => stream.getTracks().forEach((t) => t.stop());
  try {
    return record(stream, release, maxSecs);
  } catch (e) {
    // A browser that can't record still let go of the microphone.
    release();
    throw e;
  }
}

function record(stream: MediaStream, release: () => void, maxSecs: number): Recording {
  const recorder = new MediaRecorder(stream);
  const chunks: Blob[] = [];
  let cancelled = false;
  recorder.ondataavailable = (e) => chunks.push(e.data);
  const wav = new Promise<Blob | null>((resolve, reject) => {
    recorder.onstop = () => {
      clearTimeout(timer);
      release();
      if (cancelled) resolve(null);
      else toWav(new Blob(chunks, { type: recorder.mimeType })).then(resolve, reject);
    };
  });
  const stop = () => {
    if (recorder.state !== "inactive") recorder.stop();
  };
  recorder.start();
  const timer = setTimeout(stop, maxSecs * 1000);
  return {
    wav,
    stop,
    cancel: () => {
      cancelled = true;
      stop();
    },
  };
}

async function toWav(recorded: Blob): Promise<Blob> {
  const context = new AudioContext();
  let decoded: AudioBuffer;
  try {
    decoded = await context.decodeAudioData(await recorded.arrayBuffer());
  } finally {
    void context.close();
  }
  const offline = new OfflineAudioContext(1, Math.max(1, Math.ceil(decoded.duration * RATE)), RATE);
  const source = offline.createBufferSource();
  source.buffer = decoded;
  source.connect(offline.destination);
  source.start();
  const samples = (await offline.startRendering()).getChannelData(0);

  // A 44-byte PCM header, then the samples as 16-bit little-endian.
  const view = new DataView(new ArrayBuffer(44 + samples.length * 2));
  const text = (at: number, s: string) => [...s].forEach((c, i) => view.setUint8(at + i, c.charCodeAt(0)));
  text(0, "RIFF");
  view.setUint32(4, 36 + samples.length * 2, true);
  text(8, "WAVEfmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, RATE, true);
  view.setUint32(28, RATE * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, samples.length * 2, true);
  samples.forEach((s, i) => view.setInt16(44 + i * 2, Math.max(-1, Math.min(1, s)) * 0x7fff, true));
  return new Blob([view], { type: "audio/wav" });
}
