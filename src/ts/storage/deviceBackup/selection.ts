export const nativePortableDeviceSections = [
  "hypa",
  "local-plugins",
  "local-settings",
] as const;

export type NativePortableDeviceSection =
  (typeof nativePortableDeviceSections)[number];
