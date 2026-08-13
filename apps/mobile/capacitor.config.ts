import type { CapacitorConfig } from "@capacitor/cli";

const config: CapacitorConfig = {
  appId: "dev.vibex.remote",
  appName: "Vibex Remote",
  webDir: "../mobile-wasm/dist",
  server: {
    androidScheme: "https"
  }
};

export default config;
