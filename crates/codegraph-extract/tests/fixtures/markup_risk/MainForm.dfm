object MainForm: TMainForm
  Caption = 'Main'
  OnCreate = FormCreate
  object SaveButton: TButton
    Caption = 'Save'
    OnClick = SaveButtonClick
  end
  object Items: TListView
    Columns = <
      item
        Caption = 'Name'
      end>
    OnDblClick = ItemsDblClick
  end
end
