// Joint with unknown type and missing pivot.
scene { box "door" (size=[1, 2, 0.1]) }
joint "bad" (type=flapinator, axis=[0, 1, 0])
