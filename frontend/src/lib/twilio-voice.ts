/**
 * Twilio's Voice JavaScript SDK, loaded when someone first places a call rather than bundled. Twilio stopped putting 2.x
 * releases on its own CDN, so this is the jsDelivr copy of the npm release that Twilio's quickstart uses, pinned with its
 * hash. Only the part of the SDK the Calls page uses is typed here.
 */

const SDK_URL = "https://cdn.jsdelivr.net/npm/@twilio/voice-sdk@2.18.5/dist/twilio.min.js";
const SDK_INTEGRITY = "sha384-LUG7DmzDPWNKRYoqyGpV4d/IByMLiya/oUhMSc/4/6hi0wbzO/8PffM4lzeFEYS8";

export interface VoiceCall {
  on(event: "accept" | "disconnect" | "cancel" | "reject", listener: () => void): void;
  on(event: "error", listener: (error: { message?: string }) => void): void;
  disconnect(): void;
  mute(muted: boolean): void;
}

export interface VoiceDevice {
  connect(options: { params: Record<string, string> }): Promise<VoiceCall>;
  destroy(): void;
}

type VoiceSdk = { Device: new (token: string, options?: Record<string, unknown>) => VoiceDevice };

declare global {
  interface Window {
    Twilio?: VoiceSdk;
  }
}

let loading: Promise<VoiceSdk> | null = null;

export function loadVoiceSdk(): Promise<VoiceSdk> {
  if (window.Twilio?.Device) return Promise.resolve(window.Twilio);
  loading ??= new Promise<VoiceSdk>((resolve, reject) => {
    const script = document.createElement("script");
    script.src = SDK_URL;
    script.integrity = SDK_INTEGRITY;
    script.crossOrigin = "anonymous";
    script.async = true;
    script.onload = () => (window.Twilio?.Device ? resolve(window.Twilio) : reject(new Error("Twilio's calling library didn't start")));
    script.onerror = () => {
      // A failed load can be tried again on the next call.
      loading = null;
      script.remove();
      reject(new Error("Couldn't load Twilio's calling library. Check your connection and try again."));
    };
    document.head.append(script);
  });
  return loading;
}
