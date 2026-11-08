"""Timing types of saleae.data (GraphTime, GraphTimeDelta).

A GraphTime is a sample position on the capture's clock, so frames that an
HLA builds from input frame times map back to exact sample numbers.
"""

from datetime import datetime, timedelta, timezone

__all__ = ['GraphTime', 'GraphTimeDelta', 'GraphTimeInterval', 'SaleaeTime',
           'SaleaeTimeDelta', 'SaleaeTimeInterval']

# Comparisons are equal within one picosecond, as in Logic 2.
_TOLERANCE = 1e-12


class _Clock:
    """Samplerate and wall-clock time of sample 0 for one decoder run."""

    def __init__(self, samplerate, origin=None):
        self.rate = float(samplerate) if samplerate else 1.0
        self.origin = origin or datetime.now(timezone.utc)

    def at(self, sample):
        return GraphTime._at(self, sample)


# Clock used for GraphTime objects an HLA creates itself (set while it runs).
_current_clock = _Clock(1.0)


def _seconds(second=0, millisecond=0, microsecond=0, nanosecond=0,
             picosecond=0):
    return (float(second) + float(millisecond) * 1e-3 +
            float(microsecond) * 1e-6 + float(nanosecond) * 1e-9 +
            float(picosecond) * 1e-12)


class GraphTimeDelta:
    """A time difference in seconds."""

    __slots__ = ('_s',)

    def __init__(self, second=0, millisecond=0, *, microsecond=0,
                 nanosecond=0, picosecond=0):
        self._s = _seconds(second, millisecond, microsecond, nanosecond,
                           picosecond)

    @classmethod
    def _of(cls, seconds):
        delta = cls.__new__(cls)
        delta._s = float(seconds)
        return delta

    def __float__(self):
        return self._s

    def __add__(self, other):
        if isinstance(other, GraphTimeDelta):
            return GraphTimeDelta._of(self._s + other._s)
        return NotImplemented

    def __sub__(self, other):
        if isinstance(other, GraphTimeDelta):
            return GraphTimeDelta._of(self._s - other._s)
        return NotImplemented

    def __neg__(self):
        return GraphTimeDelta._of(-self._s)

    def __abs__(self):
        return GraphTimeDelta._of(abs(self._s))

    def __mul__(self, factor):
        if isinstance(factor, (int, float)):
            return GraphTimeDelta._of(self._s * factor)
        return NotImplemented

    __rmul__ = __mul__

    def __truediv__(self, other):
        if isinstance(other, GraphTimeDelta):
            return self._s / other._s
        if isinstance(other, (int, float)):
            return GraphTimeDelta._of(self._s / other)
        return NotImplemented

    def _cmp(self, other):
        if not isinstance(other, GraphTimeDelta):
            return None
        diff = self._s - other._s
        return 0 if abs(diff) <= _TOLERANCE else (1 if diff > 0 else -1)

    def __eq__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c == 0

    def __ne__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c != 0

    def __lt__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c < 0

    def __le__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c <= 0

    def __gt__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c > 0

    def __ge__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c >= 0

    def __hash__(self):
        return hash(round(self._s / _TOLERANCE))

    def __bool__(self):
        return abs(self._s) > _TOLERANCE

    def __str__(self):
        return '{:.12g} s'.format(self._s)

    def __repr__(self):
        return 'GraphTimeDelta({!r})'.format(self._s)


class GraphTime:
    """A point in time of the capture."""

    __slots__ = ('_clock', '_pos')

    def __init__(self, datetime_value, millisecond=0, *, microsecond=0,
                 nanosecond=0, picosecond=0):
        clock = _current_clock
        if datetime_value.tzinfo is None:
            datetime_value = datetime_value.replace(tzinfo=timezone.utc)
        seconds = ((datetime_value - clock.origin).total_seconds() +
                   _seconds(0, millisecond, microsecond, nanosecond,
                            picosecond))
        self._clock = clock
        self._pos = seconds * clock.rate

    @classmethod
    def _at(cls, clock, sample):
        t = cls.__new__(cls)
        t._clock = clock
        t._pos = sample
        return t

    def _seconds(self):
        return self._pos / self._clock.rate

    def _sample(self):
        return int(round(self._pos))

    def __add__(self, other):
        if isinstance(other, GraphTimeDelta):
            return GraphTime._at(self._clock,
                                 self._pos + other._s * self._clock.rate)
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, GraphTime):
            return GraphTimeDelta._of(self._seconds() - other._seconds())
        if isinstance(other, GraphTimeDelta):
            return GraphTime._at(self._clock,
                                 self._pos - other._s * self._clock.rate)
        return NotImplemented

    def _cmp(self, other):
        if not isinstance(other, GraphTime):
            return None
        diff = self._seconds() - other._seconds()
        return 0 if abs(diff) <= _TOLERANCE else (1 if diff > 0 else -1)

    def __eq__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c == 0

    def __ne__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c != 0

    def __lt__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c < 0

    def __le__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c <= 0

    def __gt__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c > 0

    def __ge__(self, other):
        c = self._cmp(other)
        return NotImplemented if c is None else c >= 0

    def __hash__(self):
        return hash(round(self._seconds() / _TOLERANCE))

    def as_datetime(self):
        return self._clock.origin + timedelta(seconds=self._seconds())

    def __str__(self):
        seconds = self._seconds()
        whole = int(seconds // 1)
        picos = int(round((seconds - whole) * 1e12))
        if picos >= 10 ** 12:
            whole, picos = whole + 1, picos - 10 ** 12
        base = self._clock.origin + timedelta(seconds=whole)
        frac = base.microsecond * 10 ** 6 + picos
        if frac >= 10 ** 12:
            base += timedelta(seconds=1)
            frac -= 10 ** 12
        return base.strftime('%Y-%m-%dT%H:%M:%S') + '.{:012d}Z'.format(frac)

    def __repr__(self):
        return 'GraphTime({})'.format(self)


class GraphTimeInterval:
    """A [begin, end] span of GraphTime."""

    def __init__(self, begin, end):
        self.begin = begin
        self.end = end

    def __repr__(self):
        return 'GraphTimeInterval({}, {})'.format(self.begin, self.end)


SaleaeTime = GraphTime
SaleaeTimeDelta = GraphTimeDelta
SaleaeTimeInterval = GraphTimeInterval
