## Multiple SNY ids

SNY07C916843009_01_07E7_C9 -> Sony Bravia with VRR disabled
SNY07CB16843009_01_07E7_C7 -> Sony Bravia with VRR enabled

## Two displays reported when only one display is active

Sometimes `displayz displayset current` will report

```
DEL430F6C19C34_34_07E8_46^SNY07CB16843009_01_07E7_C7^4416832255D8EE0306DCF64DF5DDCC6F
```

when it should actually be reporting

```
DEL430F6C19C34_34_07E8_46^F93E1AD6003508EAEAE580619857409E
```

This is either a bug in (my fork of) displayz, or it's a quirk in Windows.

Observed behaviour:

- Start with the tv on, with the External topology enabled
    - This is the `(DEL^SNY)-External` displayset-topology
    - Turning the TV off (and waiting 10s) activates the `(DEL)-Internal` displayset-topology
    - Turning the TV back on restores `(DEL^SNY)-External`

- Start with the TV on, with only the Internal topology enabled
    - This is the `(DEL^SNY)-Internal` displayset-topology
    - Turning the TV off (and waiting 10s) causes
        - the screen to go black for a second, and
        - the displayset-topology **momentarily switches to** `(DEL)-Internal` for about 36 milliseconds (5ms polling rate using `watch -i 5`)
        - the displayset-topology **remains** at `(DEL^SNY)-Internal`
        - (somehow windows isn't forgetting the SNY screen? this is strange!)
    - Turning the TV on causes
        - the screen to go black for a second, and
        - the displayset-topology **remains** at `(DEL^SNY)-Internal`


- Start with the TV on, with the External topology enabled
    - This is the `(DEL^SNY)-External` displayset-topology
    - Turning the TV off (and waiting 10s) activates the `(DEL)-Internal` displayset-topology
    - Use `displayz` to update the topology of the previous displayset:
    
        ```bash
        displayz topology set-recent 'DEL430F6C19C34_34_07E8_46^SNY07CB16843009_01_07E7_C7^4416832255D8EE0306DCF64DF5DDCC6F' Internal
        ```
    - Turning the TV on causes
        - the `(DEL^SNY)-Internal` displayset-topology to be activated
        - the screen **does not** go black


What's interesting is that there seems to be some difference between the "Recent" and the topology that is actually currently selected.
