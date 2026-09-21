# BLE Scan Filters

v0.7 uses two filter layers.

## Backend filter

Service UUID tokens are sent to `btleplug::ScanFilter.services`. Accepted input forms:

- `180D`
- `12345678`
- `0000180d-0000-1000-8000-00805f9b34fb`

16-bit and 32-bit values are expanded to the Bluetooth base UUID.

## Application post-filter

The discovered-device list independently filters on:

- name / address / peripheral id substring
- minimum RSSI
- advertised Service UUID or Service Data UUID

This second layer is intentional. Platform implementations are allowed to return additional devices, and BlueZ discovery filtering can be merged across D-Bus clients. The application therefore never assumes backend filtering is exact.

Changing the Service UUID field while a scan is already active changes the list post-filter immediately, but the backend scan filter changes only after stopping and starting the scan again.
