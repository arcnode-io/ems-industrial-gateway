use crate::add;

#[test]
fn test_add() {
    // arrange
    let expected = 3;
    // act
    let actual = add(1, 2);
    // assert
    assert_eq!(actual, expected);
}
