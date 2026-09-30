export interface DeviceSectionChoice<TSectionId extends string = string> {
  sectionId: TSectionId;
  label: string;
  selected: boolean;
  included: boolean;
  recordCount?: number;
}

export const nativePortableDeviceSections = [
  "hypa",
  "local-plugins",
  "local-settings",
] as const;

export type NativePortableDeviceSection =
  (typeof nativePortableDeviceSections)[number];

export function validateNativePortableDeviceSection(
  value: string,
): asserts value is NativePortableDeviceSection {
  if (!(nativePortableDeviceSections as readonly string[]).includes(value))
    throw new Error(`Invalid native portable device section: ${value}`);
}

export function defaultNativePortableExportChoices(): DeviceSectionChoice<NativePortableDeviceSection>[] {
  return nativePortableDeviceSections.map((sectionId) => ({
    sectionId,
    label: "",
    included: true,
    selected: sectionId !== "local-settings",
  }));
}

export function defaultNativePortableRestoreChoices(
  sections: readonly string[],
): DeviceSectionChoice<NativePortableDeviceSection>[] {
  const unique = new Set<string>();
  return sections.map((sectionId) => {
    validateNativePortableDeviceSection(sectionId);
    if (unique.has(sectionId))
      throw new Error("Duplicate native portable device section");
    unique.add(sectionId);
    return {
      sectionId,
      label: "",
      included: true,
      selected: true,
    };
  });
}
