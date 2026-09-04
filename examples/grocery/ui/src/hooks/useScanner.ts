import { useState, useCallback, useRef } from "react";
import { ScanResult, Role } from "../types/grocery";
import * as api from "../api/client";

export interface UseScannerOptions {
  activeRole?: Role;
  showToast?: (msg: string) => void;
  onScanSuccess?: (result: ScanResult) => void;
}

export function useScanner(options?: UseScannerOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const [scanning, setScanning] = useState(false);
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [scanError, setScanError] = useState<string | null>(null);

  const handleScanPngBytes = useCallback(
    async (bytes: ArrayBuffer | Uint8Array, _name?: string) => {
      setScanning(true);
      setScanError(null);
      setScanResult(null);

      const activeRole = optionsRef.current?.activeRole || "shopper";
      try {
        const result = await api.scanBarcodeBytes(bytes, activeRole);
        setScanResult(result);
        optionsRef.current?.showToast?.(
          `Decoded: ${result.barcode.text} (${result.barcode.symbology.toUpperCase()})`
        );
        optionsRef.current?.onScanSuccess?.(result);
      } catch (err: any) {
        setScanError(err.message || "Failed to decode barcode.");
      } finally {
        setScanning(false);
      }
    },
    []
  );

  const handleTestFixture = useCallback(
    async (fixtureName: string) => {
      try {
        setScanning(true);
        const buf = await api.fetchFixtureBytes(fixtureName);
        await handleScanPngBytes(buf, fixtureName);
      } catch (err: any) {
        setScanError(`Fixture error: ${err.message}`);
        setScanning(false);
      }
    },
    [handleScanPngBytes]
  );

  const handleFileUpload = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const file = e.target.files?.[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = () => {
        if (reader.result instanceof ArrayBuffer) {
          handleScanPngBytes(reader.result, file.name);
        }
      };
      reader.readAsArrayBuffer(file);
      e.target.value = "";
    },
    [handleScanPngBytes]
  );

  return {
    scanning,
    scanResult,
    setScanResult,
    scanError,
    setScanError,
    handleScanPngBytes,
    handleTestFixture,
    handleFileUpload,
  };
}
