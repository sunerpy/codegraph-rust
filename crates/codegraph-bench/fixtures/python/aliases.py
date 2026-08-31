from pkg import module as mod_alias
import top_level as tl
from imported_types import ImportedClass as ImportedAlias


def from_import_caller():
    return mod_alias.func()


def plain_import_caller():
    return tl.top_func()


def member_alias_value():
    return ImportedAlias
