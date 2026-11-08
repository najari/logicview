"""HLA settings, declared as class attributes of a HighLevelAnalyzer."""


class ValidationError(RuntimeError):
    pass


def _check(condition, message):
    if not condition:
        raise ValidationError(message)


class Setting:
    def __init__(self, *, label=None):
        self.label = label
        self.name = None

    def validate(self, value):
        pass


class StringSetting(Setting):
    """Free text."""

    def validate(self, value):
        _check(isinstance(value, str), 'Must be a string')


class NumberSetting(Setting):
    """An int or float, optionally limited to [min_value, max_value]."""

    def __init__(self, *, min_value=None, max_value=None, **kwargs):
        super().__init__(**kwargs)
        if min_value is not None and max_value is not None:
            _check(min_value <= max_value,
                   'min_value cannot be greater than max_value')
        self.min_value = min_value
        self.max_value = max_value

    def validate(self, value):
        _check(isinstance(value, (int, float)) and not isinstance(value, bool),
               'Must be an int or float')
        _check(self.min_value is None or value >= self.min_value,
               'Number less than {}'.format(self.min_value))
        _check(self.max_value is None or value <= self.max_value,
               'Number greater than {}'.format(self.max_value))


class ChoicesSetting(Setting):
    """One string out of a fixed list."""

    def __init__(self, choices, **kwargs):
        super().__init__(**kwargs)
        choices = list(choices)
        for choice in choices:
            _check(isinstance(choice, str), 'Choice must be a string')
        self.choices = choices

    def validate(self, value):
        _check(isinstance(value, str), 'Must be a string')
        _check(value in self.choices, 'Invalid choice')
