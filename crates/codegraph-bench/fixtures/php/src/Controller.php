<?php

namespace App;

use App\Field;
use App\Field\FirstName;

class LocaleDefinition
{
    public function trans(string $key): string
    {
        return $key;
    }
}

class Controller extends Base
{
    public function show(): array
    {
        $name = Field\FirstName::make();
        $other = FirstName::make();
        return [
            'label' => trans('title'),
            'price' => formatprice(300),
            'name' => $name->render() . $other->render(),
        ];
    }

    public function parentCall(): string
    {
        return parent::describe();
    }
}

class Base
{
    public function describe(): string
    {
        return 'base';
    }
}
