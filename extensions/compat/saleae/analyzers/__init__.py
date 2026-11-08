"""HighLevelAnalyzer base class and AnalyzerFrame."""

from .settings import (Setting, StringSetting, NumberSetting, ChoicesSetting,
                       ValidationError)

__all__ = ['HighLevelAnalyzer', 'AnalyzerFrame', 'StringSetting',
           'NumberSetting', 'ChoicesSetting', 'ValidationError']


def _label_from_name(name):
    words = [w for w in name.replace('_', ' ').split(' ') if w]
    return ' '.join(w[0].upper() + w[1:] for w in words)


class _HlaMeta(type):
    # Logic 2 creates an HLA as cls(settings): the settings are set as
    # instance attributes before the HLA's own __init__() runs without them.
    def __call__(cls, settings, *args, **kwargs):
        obj = cls.__new__(cls, settings)
        obj.__init__(*args, **kwargs)
        return obj


class HighLevelAnalyzer(metaclass=_HlaMeta):
    """Base class of HLA extensions: declare settings and result_types as
    class attributes and implement decode(frame)."""

    result_types = {}

    @classmethod
    def _get_settings(cls):
        """Settings in declaration order (base classes first)."""
        if '_hla_settings' in cls.__dict__:
            return cls._hla_settings
        found = {}
        for klass in reversed(cls.__mro__):
            for name, value in vars(klass).items():
                if isinstance(value, Setting):
                    found.pop(name, None)
                    found[name] = value
        settings = []
        for name, value in found.items():
            value.name = name
            if value.label is None:
                value.label = _label_from_name(name)
            settings.append(value)
        cls._hla_settings = settings
        return settings

    def __new__(cls, settings, *args, **kwargs):
        obj = super().__new__(cls)
        for setting in cls._get_settings():
            if setting.name not in settings:
                raise ValidationError('Missing setting: ' + setting.name)
            value = settings[setting.name]
            setting.validate(value)
            setattr(obj, setting.name, value)
        return obj

    def decode(self, frame):
        raise NotImplementedError('HLA must implement decode()')


class AnalyzerFrame:
    """One frame: a type name, a time span and a data dict."""

    def __init__(self, type, start_time, end_time, data=None):
        self.type = type
        self.start_time = start_time
        self.end_time = end_time
        self.data = data

    def __repr__(self):
        return 'AnalyzerFrame({!r}, {}, {}, {!r})'.format(
            self.type, self.start_time, self.end_time, self.data)
